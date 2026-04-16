use crate::clip::TimelineClip;
use crate::id::{ClipId, LayerId, MarkerId};
use crate::layer::Layer;
use crate::marker::TimelineMarker;
use crate::units::Beats;
use ahash::AHashMap;
use serde::{Deserialize, Serialize};

/// The timeline containing all layers and clips.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Timeline {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub layers: Vec<Layer>,
    #[serde(default)]
    pub export_in_beat: Beats,
    #[serde(default)]
    pub export_out_beat: Beats,
    #[serde(default)]
    pub export_range_enabled: bool,

    /// User-placed timeline markers (sorted by beat on insert).
    #[serde(default)]
    pub markers: Vec<TimelineMarker>,

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
            if self
                .layers
                .get(li)
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
    pub fn register_clip_in_lookup(
        &mut self,
        clip_id: &str,
        layer_index: usize,
        clip_index: usize,
    ) {
        self.clip_lookup
            .insert(ClipId::new(clip_id), (layer_index, clip_index));
    }

    /// Get total duration in beats (max clip EndBeat across all layers).
    pub fn duration_beats(&self) -> Beats {
        let mut max_beat = Beats::ZERO;
        for layer in &self.layers {
            for clip in &layer.clips {
                max_beat = max_beat.max(clip.end_beat());
            }
        }
        max_beat
    }

    /// Content range in beats: (start_beat, end_beat).
    /// If export markers are set, clamps to them.
    /// Otherwise returns first-clip-start to last-clip-end.
    /// Port of Unity Timeline.GetContentRange().
    pub fn content_range_beats(&self) -> (Beats, Beats) {
        let mut min_beat = Beats(f64::MAX);
        let mut max_beat = Beats::ZERO;
        for layer in &self.layers {
            for clip in &layer.clips {
                min_beat = min_beat.min(clip.start_beat);
                max_beat = max_beat.max(clip.end_beat());
            }
        }
        if min_beat.0 >= f64::MAX / 2.0 {
            return (Beats::ZERO, Beats::ZERO);
        }

        // Apply export markers if set
        if self.export_range_enabled {
            if self.export_in_beat > Beats::ZERO {
                min_beat = min_beat.max(self.export_in_beat);
            }
            if self.export_out_beat > Beats::ZERO {
                max_beat = max_beat.min(self.export_out_beat);
            }
        }

        (min_beat, max_beat)
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
        self.enforce_tree_order();
    }

    /// Remove a layer at the given index, reindexing remaining layers.
    pub fn remove_layer(&mut self, index: usize) -> Option<Layer> {
        if index >= self.layers.len() {
            return None;
        }
        let layer = self.layers.remove(index);
        self.enforce_tree_order();
        self.mark_clip_lookup_dirty();
        Some(layer)
    }

    /// Atomically replace the entire layer order.
    pub fn replace_layer_order(&mut self, new_order: Vec<Layer>) {
        self.layers = new_order;
        self.enforce_tree_order();
        self.mark_clip_lookup_dirty();
    }

    /// Reorder `self.layers` into pre-order DFS traversal of the parent→child
    /// forest, so every group is immediately followed by its descendants
    /// (preserving sibling order). Idempotent and stable.
    ///
    /// Invariant: for every layer L, all layers whose `parent_layer_id == L.layer_id`
    /// form a contiguous run immediately after L (recursively). The flat
    /// `layers` Vec is a serialization of the layer tree.
    ///
    /// Orphans (children whose parent_layer_id points to a missing layer) are
    /// promoted to roots in their original position to avoid data loss.
    ///
    /// Always finishes by calling `reindex_layers()`.
    pub fn enforce_tree_order(&mut self) {
        let n = self.layers.len();
        if n == 0 {
            self.reindex_layers();
            return;
        }

        // Build children map (parent_id → ordered child indices) and collect
        // root indices in their original order. A "root" is any layer with no
        // parent_layer_id, or whose parent_layer_id does not match an existing
        // layer in this timeline (orphan rescue).
        let id_set: ahash::AHashSet<&LayerId> =
            self.layers.iter().map(|l| &l.layer_id).collect();

        let mut children: AHashMap<LayerId, Vec<usize>> = AHashMap::new();
        let mut roots: Vec<usize> = Vec::with_capacity(n);
        for (i, layer) in self.layers.iter().enumerate() {
            match &layer.parent_layer_id {
                Some(pid) if id_set.contains(pid) => {
                    children.entry(pid.clone()).or_default().push(i);
                }
                _ => roots.push(i),
            }
        }

        // Pre-order DFS, iterative to support arbitrary nesting safely.
        let mut new_order: Vec<usize> = Vec::with_capacity(n);
        let mut stack: Vec<usize> = roots.iter().rev().copied().collect();
        let mut visited = vec![false; n];
        while let Some(idx) = stack.pop() {
            if visited[idx] {
                continue;
            }
            visited[idx] = true;
            new_order.push(idx);
            let layer_id = &self.layers[idx].layer_id;
            if let Some(child_idxs) = children.get(layer_id) {
                for &child_idx in child_idxs.iter().rev() {
                    if !visited[child_idx] {
                        stack.push(child_idx);
                    }
                }
            }
        }

        // Safety net: if any layer was somehow not visited (cycles, etc.),
        // append it at the end as a root rather than dropping it.
        if new_order.len() != n {
            for (i, &was_visited) in visited.iter().enumerate() {
                if !was_visited {
                    new_order.push(i);
                }
            }
        }

        // Fast path: already in tree order — skip the rebuild.
        if new_order.iter().enumerate().all(|(new_i, &old_i)| new_i == old_i) {
            self.reindex_layers();
            return;
        }

        // Permute self.layers into the new order without cloning Layer.
        let mut taken: Vec<Option<Layer>> = self.layers.drain(..).map(Some).collect();
        let mut reordered: Vec<Layer> = Vec::with_capacity(n);
        for &old_i in &new_order {
            reordered.push(taken[old_i].take().expect("each index visited once"));
        }
        self.layers = reordered;
        self.reindex_layers();
    }

    /// Debug-only invariant check: every group's children form a contiguous
    /// run immediately after the group in `self.layers`. Panics on violation.
    #[cfg(debug_assertions)]
    pub fn debug_assert_tree_order(&self) {
        let id_to_index: AHashMap<&LayerId, usize> = self
            .layers
            .iter()
            .enumerate()
            .map(|(i, l)| (&l.layer_id, i))
            .collect();
        for (i, layer) in self.layers.iter().enumerate() {
            if let Some(pid) = &layer.parent_layer_id
                && let Some(&parent_i) = id_to_index.get(pid)
            {
                assert!(
                    parent_i < i,
                    "tree-order violation: child '{}' at {} appears before parent '{}' at {}",
                    layer.layer_id.as_str(),
                    i,
                    pid.as_str(),
                    parent_i,
                );
                // All layers between parent_i+1 and i must be descendants of parent_i.
                for between in (parent_i + 1)..i {
                    let mut cur = self.layers[between].parent_layer_id.as_ref();
                    let mut ok = false;
                    while let Some(c) = cur {
                        if c == pid {
                            ok = true;
                            break;
                        }
                        cur = id_to_index
                            .get(c)
                            .and_then(|&idx| self.layers[idx].parent_layer_id.as_ref());
                    }
                    assert!(
                        ok,
                        "tree-order violation: non-descendant '{}' at {} sits between parent '{}' at {} and child '{}' at {}",
                        self.layers[between].layer_id.as_str(),
                        between,
                        pid.as_str(),
                        parent_i,
                        layer.layer_id.as_str(),
                        i,
                    );
                }
            }
        }
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
    /// Ensures sort caches are up-to-date, then queries into caller-provided buffer.
    /// From Unity Timeline.cs GetActiveClipsAtBeat (lines 331-374).
    pub fn get_active_clips_at_beat(
        &mut self,
        beat: Beats,
        results: &mut Vec<(usize, usize)>,
    ) {
        self.ensure_layers_sorted();
        self.get_active_clips_at_beat_ref(beat, results);
    }

    /// Get active clips at a given beat into caller-provided buffer.
    /// IMPORTANT: Caller must ensure sort caches are current via `ensure_layers_sorted()`
    /// before calling this. Use `get_active_clips_at_beat()` if unsure.
    /// Zero per-frame allocation — uses caller's pre-allocated buffer.
    pub fn get_active_clips_at_beat_ref(
        &self,
        beat: Beats,
        results: &mut Vec<(usize, usize)>,
    ) {
        let any_solo = self.layers.iter().any(|l| l.is_solo);
        results.clear();
        let mut active_indices = Vec::new();

        for li in 0..self.layers.len() {
            if self.layers[li].is_group() {
                continue;
            }

            if self.layers[li].is_muted {
                continue;
            }

            if self.layers[li].parent_layer_id.is_some() {
                let parent = self.find_group_parent(li);
                let parent_muted = parent.map(|(_, p)| p.is_muted).unwrap_or(false);
                if parent_muted {
                    continue;
                }

                if any_solo
                    && !self.layers[li].is_solo
                    && !parent.map(|(_, p)| p.is_solo).unwrap_or(false)
                {
                    continue;
                }
            } else if any_solo && !self.layers[li].is_solo {
                continue;
            }

            active_indices.clear();
            self.layers[li]
                .collect_active_clips_at_beat(beat, &mut active_indices);
            for ci in &active_indices {
                if !self.layers[li].clips[*ci].is_muted {
                    results.push((li, *ci));
                }
            }
        }
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
        self.layers
            .iter()
            .enumerate()
            .find(|(_, l)| l.layer_id == layer_id)
    }

    /// Find layer index by persistent ID. Convenience wrapper.
    pub fn find_layer_index_by_id(&self, layer_id: &str) -> Option<usize> {
        self.find_layer_by_id(layer_id).map(|(i, _)| i)
    }

    /// Find layer by persistent ID (mutable). Returns (index, &mut Layer).
    pub fn find_layer_by_id_mut(&mut self, layer_id: &str) -> Option<(usize, &mut Layer)> {
        self.layers
            .iter_mut()
            .enumerate()
            .find(|(_, l)| l.layer_id == layer_id)
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
        (self.duration_beats().0 * seconds_per_beat as f64) as f32
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
    pub fn get_start_beat(&self) -> Beats {
        let mut min_beat = Beats(f64::MAX);
        for layer in &self.layers {
            for clip in &layer.clips {
                if clip.start_beat < min_beat {
                    min_beat = clip.start_beat;
                }
            }
        }
        if min_beat.0 >= f64::MAX / 2.0 {
            Beats::ZERO
        } else {
            min_beat
        }
    }

    // ── Markers ─────────────────────────────────────────────────────

    /// Add a marker, maintaining sorted order by beat.
    pub fn add_marker(&mut self, marker: TimelineMarker) {
        let pos = self
            .markers
            .binary_search_by(|m| {
                m.beat
                    .partial_cmp(&marker.beat)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or_else(|i| i);
        self.markers.insert(pos, marker);
    }

    /// Remove a marker by ID. Returns the removed marker if found.
    pub fn remove_marker(&mut self, id: &MarkerId) -> Option<TimelineMarker> {
        let pos = self.markers.iter().position(|m| m.id == *id)?;
        Some(self.markers.remove(pos))
    }

    /// Find a marker by ID (immutable).
    pub fn find_marker(&self, id: &MarkerId) -> Option<&TimelineMarker> {
        self.markers.iter().find(|m| m.id == *id)
    }

    /// Find a marker by ID (mutable).
    pub fn find_marker_mut(&mut self, id: &MarkerId) -> Option<&mut TimelineMarker> {
        self.markers.iter_mut().find(|m| m.id == *id)
    }

    /// Re-sort markers after a beat change.
    pub fn sort_markers(&mut self) {
        self.markers.sort_by(|a, b| {
            a.beat
                .partial_cmp(&b.beat)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
}

#[cfg(test)]
mod enforce_tree_order_tests {
    use super::*;
    use crate::layer::Layer;
    use crate::types::LayerType;

    fn mk(name: &str) -> Layer {
        Layer::new(name.into(), LayerType::Video, 0)
    }
    fn mk_group(name: &str) -> Layer {
        Layer::new(name.into(), LayerType::Group, 0)
    }
    fn names(t: &Timeline) -> Vec<&str> {
        t.layers.iter().map(|l| l.name.as_str()).collect()
    }

    fn parent(child: &mut Layer, parent: &Layer) {
        child.parent_layer_id = Some(parent.layer_id.clone());
    }

    #[test]
    fn reproduces_screenshot_bug() {
        // Flat order: BASALT, Group, Gen19, Gen14, ChildA, ChildB
        // ChildA/ChildB are children of Group but appear after non-children.
        let mut t = Timeline::default();
        let basalt = mk("BASALT");
        let group = mk_group("Group");
        let gen19 = mk("Gen19");
        let gen14 = mk("Gen14");
        let mut child_a = mk("ChildA");
        let mut child_b = mk("ChildB");
        parent(&mut child_a, &group);
        parent(&mut child_b, &group);
        t.layers = vec![basalt, group, gen19, gen14, child_a, child_b];

        t.enforce_tree_order();

        assert_eq!(
            names(&t),
            vec!["BASALT", "Group", "ChildA", "ChildB", "Gen19", "Gen14"]
        );
    }

    #[test]
    fn idempotent() {
        let mut t = Timeline::default();
        let group = mk_group("G");
        let mut a = mk("A");
        let mut b = mk("B");
        parent(&mut a, &group);
        parent(&mut b, &group);
        t.layers = vec![mk("Top"), group, mk("Mid"), a, b, mk("Bot")];
        t.enforce_tree_order();
        let after_first: Vec<String> =
            t.layers.iter().map(|l| l.name.clone()).collect();
        t.enforce_tree_order();
        let after_second: Vec<String> =
            t.layers.iter().map(|l| l.name.clone()).collect();
        assert_eq!(after_first, after_second);
    }

    #[test]
    fn already_ordered_is_unchanged() {
        let mut t = Timeline::default();
        let group = mk_group("G");
        let mut a = mk("A");
        let mut b = mk("B");
        parent(&mut a, &group);
        parent(&mut b, &group);
        t.layers = vec![mk("Top"), group, a, b, mk("Bot")];
        t.enforce_tree_order();
        assert_eq!(names(&t), vec!["Top", "G", "A", "B", "Bot"]);
    }

    #[test]
    fn two_interleaved_groups() {
        let mut t = Timeline::default();
        let g1 = mk_group("G1");
        let g2 = mk_group("G2");
        let mut a1 = mk("A1");
        let mut a2 = mk("A2");
        let mut b1 = mk("B1");
        let mut b2 = mk("B2");
        parent(&mut a1, &g1);
        parent(&mut a2, &g1);
        parent(&mut b1, &g2);
        parent(&mut b2, &g2);
        // Interleaved: G1, G2, A1, B1, A2, B2
        t.layers = vec![g1, g2, a1, b1, a2, b2];
        t.enforce_tree_order();
        assert_eq!(names(&t), vec!["G1", "A1", "A2", "G2", "B1", "B2"]);
    }

    #[test]
    fn nested_groups_pre_order() {
        let mut t = Timeline::default();
        let outer = mk_group("Outer");
        let inner = mk_group("Inner");
        let mut leaf1 = mk("Leaf1");
        let mut leaf2 = mk("Leaf2");
        let mut sib = mk("Sib");
        let mut inner_clone = inner.clone();
        parent(&mut inner_clone, &outer);
        parent(&mut leaf1, &inner_clone);
        parent(&mut leaf2, &inner_clone);
        parent(&mut sib, &outer);
        // Scrambled order
        t.layers = vec![leaf1, sib, outer, mk("After"), inner_clone, leaf2];
        t.enforce_tree_order();
        // Outer's children appear in their original relative order (Sib at
        // idx 1 came before Inner at idx 4 in the input).
        assert_eq!(
            names(&t),
            vec!["Outer", "Sib", "Inner", "Leaf1", "Leaf2", "After"]
        );
    }

    #[test]
    fn orphan_child_kept_as_root() {
        let mut t = Timeline::default();
        let mut orphan = mk("Orphan");
        // parent_layer_id points to a layer that does not exist
        orphan.parent_layer_id = Some(crate::id::LayerId::from("ghost".to_string()));
        t.layers = vec![mk("Top"), orphan, mk("Bot")];
        t.enforce_tree_order();
        // Orphan is treated as a root and not lost
        assert_eq!(names(&t), vec!["Top", "Orphan", "Bot"]);
    }

    #[test]
    fn empty_timeline_is_safe() {
        let mut t = Timeline::default();
        t.enforce_tree_order();
        assert!(t.layers.is_empty());
    }
}
