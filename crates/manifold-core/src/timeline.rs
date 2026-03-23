use serde::{Deserialize, Serialize};
use ahash::AHashMap;
use crate::id::{ClipId, LayerId};
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
    clip_lookup: AHashMap<ClipId, (usize, usize)>,
    #[serde(skip)]
    clip_lookup_dirty: bool,

    /// Runtime layer_id → position index map. Rebuilt in reindex_layers().
    #[serde(skip)]
    layer_id_to_index: AHashMap<LayerId, usize>,
}

impl Timeline {
    /// Read-only access to the layer_id → index cache (for batch lookups in sort).
    #[inline]
    pub fn layer_id_index_map(&self) -> &AHashMap<LayerId, usize> {
        &self.layer_id_to_index
    }

    /// Resolve a LayerId to its current positional index.
    pub fn layer_index_for_id(&self, id: &LayerId) -> Option<usize> {
        // Fast path: use cached map if populated
        if let Some(&idx) = self.layer_id_to_index.get(id) {
            return Some(idx);
        }
        // Fallback: linear scan (before first reindex_layers call)
        self.layers.iter().position(|l| l.layer_id == *id)
    }

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
        self.clip_lookup.insert(ClipId::new(clip_id), (layer_index, clip_index));
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

    /// Insert a new layer at the given index, reindexing all layers.
    pub fn insert_layer(&mut self, index: usize, mut layer: Layer) {
        let idx = index.min(self.layers.len());
        layer.index = idx as i32;
        self.layers.insert(idx, layer);
        self.reindex_layers();
    }

    /// Remove a layer at the given index, reindexing remaining layers.
    pub fn remove_layer(&mut self, index: usize) -> Option<Layer> {
        if index >= self.layers.len() {
            return None;
        }
        let layer = self.layers.remove(index);
        self.reindex_layers();
        self.mark_clip_lookup_dirty();
        Some(layer)
    }

    /// Atomically replace the entire layer order.
    pub fn replace_layer_order(&mut self, new_order: Vec<Layer>) {
        self.layers = new_order;
        self.reindex_layers();
        self.mark_clip_lookup_dirty();
    }

    /// Add a named layer with the given type and optional generator type.
    /// Port of Unity Timeline.cs AddLayer overloads.
    /// Returns the index of the newly created layer.
    pub fn add_layer(
        &mut self,
        name: &str,
        layer_type: crate::types::LayerType,
        generator_type: crate::generator_type_id::GeneratorTypeId,
    ) -> usize {
        let idx = self.layers.len();
        let mut layer = Layer::new(name.to_string(), layer_type, idx as i32);
        if generator_type != crate::generator_type_id::GeneratorTypeId::NONE {
            layer.change_generator_type(generator_type);
        }
        self.layers.push(layer);
        idx
    }

    /// Add a default video layer, returning its index.
    pub fn add_layer_default(&mut self) -> usize {
        let idx = self.layers.len();
        let layer = Layer::new(
            format!("Layer {}", idx + 1),
            crate::types::LayerType::Video,
            idx as i32,
        );
        self.layers.push(layer);
        idx
    }

    /// Ensure at least `count` layers exist (used by LiveClipManager).
    pub fn ensure_layer_count(&mut self, count: usize) {
        while self.layers.len() < count {
            self.add_layer_default();
        }
    }

    /// Reindex all layers and their clips after structural changes.
    /// Also call after deserialization to populate `layer_id_to_index`.
    pub fn reindex_layers(&mut self) {
        self.layer_id_to_index.clear();
        for (i, layer) in self.layers.iter_mut().enumerate() {
            layer.index = i as i32;
            self.layer_id_to_index.insert(layer.layer_id.clone(), i);
            for clip in &mut layer.clips {
                clip.layer_id = layer.layer_id.clone();
            }
        }
        self.mark_clip_lookup_dirty();
    }

    /// Ensure all layers have up-to-date sort caches. Call before `get_active_clips_at_beat_ref`.
    pub fn ensure_layers_sorted(&mut self) {
        for layer in &mut self.layers {
            layer.ensure_sorted();
        }
    }

    /// Get active clips at a given beat (respecting mute/solo with group hierarchy).
    /// This &mut self variant ensures sort caches are up-to-date before querying.
    /// From Unity Timeline.cs GetActiveClipsAtBeat (lines 331-374).
    pub fn get_active_clips_at_beat(&mut self, beat: f32) -> Vec<(usize, usize)> {
        self.ensure_layers_sorted();
        self.get_active_clips_at_beat_ref(beat)
    }

    /// Get active clips at a given beat (&self variant).
    /// IMPORTANT: Caller must ensure sort caches are current via `ensure_layers_sorted()`
    /// before calling this. Use `get_active_clips_at_beat()` if unsure.
    pub fn get_active_clips_at_beat_ref(&self, beat: f32) -> Vec<(usize, usize)> {
        let any_solo = self.layers.iter().any(|l| l.is_solo);
        let mut results = Vec::new();

        for li in 0..self.layers.len() {
            if self.layers[li].is_group() {
                continue;
            }

            if self.layers[li].is_muted {
                continue;
            }

            if self.layers[li].parent_layer_id.is_some() {
                let parent_muted = self.find_group_parent(li)
                    .map(|(_, p)| p.is_muted)
                    .unwrap_or(false);
                if parent_muted {
                    continue;
                }

                let parent_solo = self.find_group_parent(li)
                    .map(|(_, p)| p.is_solo)
                    .unwrap_or(false);

                if any_solo && !self.layers[li].is_solo && !parent_solo {
                    continue;
                }
            } else {
                if any_solo && !self.layers[li].is_solo {
                    continue;
                }
            }

            let mut active_indices = Vec::new();
            self.layers[li].collect_active_clips_at_beat(beat, &mut active_indices);
            for ci in active_indices {
                if !self.layers[li].clips[ci].is_muted {
                    results.push((li, ci));
                }
            }
        }

        results
    }

    /// Find the parent group layer for a child at the given flat index.
    /// Single-depth: scans backward since parent appears before children.
    /// From Unity Timeline.cs FindGroupParent (lines 380-390).
    pub fn find_group_parent(&self, child_index: usize) -> Option<(usize, &Layer)> {
        let parent_id = self.layers.get(child_index)?.parent_layer_id.as_ref()?;
        for i in (0..child_index).rev() {
            if self.layers[i].layer_id == *parent_id {
                return Some((i, &self.layers[i]));
            }
        }
        None
    }

    /// Find layer by persistent ID. Unity Timeline.cs lines 225-234.
    pub fn find_layer_by_id(&self, layer_id: &str) -> Option<(usize, &Layer)> {
        self.layers.iter().enumerate().find(|(_, l)| l.layer_id == layer_id)
    }

    /// Find layer index by persistent ID. Convenience wrapper.
    pub fn find_layer_index_by_id(&self, layer_id: &str) -> Option<usize> {
        self.find_layer_by_id(layer_id).map(|(i, _)| i)
    }

    /// Find layer by persistent ID (mutable). Returns (index, &mut Layer).
    pub fn find_layer_by_id_mut(&mut self, layer_id: &str) -> Option<(usize, &mut Layer)> {
        self.layers.iter_mut().enumerate().find(|(_, l)| l.layer_id == layer_id)
    }

    /// Move layer from one index to another. Unity Timeline.cs lines 250-266.
    pub fn move_layer(&mut self, from: usize, to: usize) {
        if from >= self.layers.len() || to >= self.layers.len() || from == to {
            return;
        }
        let layer = self.layers.remove(from);
        self.layers.insert(to, layer);
        self.reindex_layers();
    }

    /// Get duration in seconds. Unity Timeline.cs lines 105-108.
    pub fn get_duration_seconds(&self, seconds_per_beat: f32) -> f32 {
        self.duration_beats() * seconds_per_beat
    }

    /// Clear all clips on all layers. Unity Timeline.cs lines 439-445.
    pub fn clear_all_clips(&mut self) {
        for layer in &mut self.layers {
            layer.clear_clips();
        }
        self.mark_clip_lookup_dirty();
    }

    /// Insert an existing pre-built layer at index. Unity Timeline.cs lines 190-196.
    pub fn insert_existing_layer(&mut self, index: usize, layer: Layer) {
        let idx = index.min(self.layers.len());
        self.layers.insert(idx, layer);
        self.reindex_layers();
    }

    /// Get the earliest clip start beat across all layers.
    /// From Unity Timeline.cs GetStartBeat.
    pub fn get_start_beat(&self) -> f32 {
        let mut min_beat = f32::MAX;
        for layer in &self.layers {
            for clip in &layer.clips {
                if clip.start_beat < min_beat {
                    min_beat = clip.start_beat;
                }
            }
        }
        min_beat
    }
}
