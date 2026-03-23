use manifold_core::{ClipId, LayerId};
use manifold_core::clip::TimelineClip;
use manifold_core::project::Project;
use manifold_core::timeline::Timeline;
use ahash::AHashMap;

/// Runtime active-clip window for timeline playback.
/// Maintains an incremental time-domain set of active clips and advances via
/// clip boundary cursors, so per-frame work scales with boundary crossings.
/// Port of C# ActiveTimelineClipWindow.
pub struct ActiveTimelineClipWindow {
    clips_by_start: Vec<TimelineClip>,
    clips_by_end: Vec<TimelineClip>,
    /// O(1) active lookup by clip id.
    active_by_id: AHashMap<ClipId, TimelineClip>,
    /// Parallel iteration list — avoids HashMap values enumerator allocation.
    /// PERF: The HashMap is retained for O(1) keyed lookup in advance_active_set_forward.
    /// DO NOT REMOVE. Kept in sync with active_by_id at all times.
    active_by_id_values: Vec<TimelineClip>,
    visible_scratch: Vec<TimelineClip>,
    layer_id_to_index: AHashMap<LayerId, usize>,
    visible_layers: Vec<bool>,
    indexed_layer_clip_counts: Vec<usize>,

    initialized: bool,
    last_beat: f32,
    start_cursor: usize,
    end_cursor: usize,
}

const BACKWARD_EPSILON: f32 = 0.0001;
const LARGE_JUMP_BEATS: f32 = 32.0;

impl ActiveTimelineClipWindow {
    pub fn new() -> Self {
        Self {
            clips_by_start: Vec::with_capacity(256),
            clips_by_end: Vec::with_capacity(256),
            active_by_id: AHashMap::with_capacity(256),
            active_by_id_values: Vec::with_capacity(256),
            visible_scratch: Vec::with_capacity(128),
            layer_id_to_index: AHashMap::with_capacity(32),
            visible_layers: Vec::new(),
            indexed_layer_clip_counts: Vec::new(),
            initialized: false,
            last_beat: 0.0,
            start_cursor: 0,
            end_cursor: 0,
        }
    }

    /// Clear all state. Unity ActiveTimelineClipWindow.Reset (lines 41-53).
    pub fn reset(&mut self) {
        self.initialized = false;
        self.last_beat = 0.0;
        self.start_cursor = 0;
        self.end_cursor = 0;
        self.indexed_layer_clip_counts.clear();
        self.clips_by_start.clear();
        self.clips_by_end.clear();
        self.active_by_id.clear();
        self.active_by_id_values.clear();
    }

    /// Main entry point. Fills `results` with visible active clips at `beat`.
    /// Unity ActiveTimelineClipWindow.GetActiveClips (lines 55-79).
    pub fn get_active_clips(
        &mut self,
        project: &Project,
        beat: f32,
        results: &mut Vec<TimelineClip>,
    ) {
        results.clear();
        if project.timeline.layers.is_empty() {
            return;
        }

        self.ensure_index(project);

        if !self.initialized
            || beat + BACKWARD_EPSILON < self.last_beat
            || (beat - self.last_beat) > LARGE_JUMP_BEATS
        {
            self.rebuild_active_set_at_beat(beat);
        } else {
            self.advance_active_set_forward(beat);
        }

        self.last_beat = beat;
        self.initialized = true;
        self.collect_visible(&project.timeline, beat, results);
    }

    /// Invalidate and rebuild the sorted index if the project structure changed.
    /// Unity ActiveTimelineClipWindow.EnsureIndex (lines 81-89).
    fn ensure_index(&mut self, project: &Project) {
        if !self.is_index_valid(project) {
            self.build_index(project);
            self.initialized = false;
        }
    }

    /// Check whether the sorted clip index is still valid.
    /// Unity ActiveTimelineClipWindow.IsIndexValid (lines 91-108).
    /// Note: Unity checks reference identity (indexedProject != project). In Rust there is no
    /// struct reference identity — validity is determined entirely by per-layer clip counts,
    /// which captures all structural changes (add/remove clip or layer).
    fn is_index_valid(&self, project: &Project) -> bool {
        let layers = &project.timeline.layers;
        if self.indexed_layer_clip_counts.len() != layers.len() {
            return false;
        }
        for (i, layer) in layers.iter().enumerate() {
            if self.indexed_layer_clip_counts[i] != layer.clips.len() {
                return false;
            }
        }
        true
    }

    /// Rebuild the sorted clip index from the current project state.
    /// Unity ActiveTimelineClipWindow.BuildIndex (lines 110-145).
    fn build_index(&mut self, project: &Project) {
        let layers = &project.timeline.layers;

        // Build layer_id → index map for sort comparators
        self.layer_id_to_index.clear();
        for (i, layer) in layers.iter().enumerate() {
            if !layer.layer_id.is_empty() {
                self.layer_id_to_index.insert(layer.layer_id.clone(), i);
            }
        }

        // Store per-layer clip counts
        self.indexed_layer_clip_counts.clear();
        for layer in layers {
            self.indexed_layer_clip_counts.push(layer.clips.len());
        }

        // Collect all clips from non-group layers
        self.clips_by_start.clear();
        for layer in layers {
            if layer.is_group() {
                continue;
            }
            for clip in &layer.clips {
                self.clips_by_start.push(clip.clone());
            }
        }

        let id_map = &self.layer_id_to_index;
        self.clips_by_start.sort_by(|a, b| compare_start_order(a, b, id_map));

        self.clips_by_end.clear();
        self.clips_by_end.extend_from_slice(&self.clips_by_start);
        self.clips_by_end.sort_by(|a, b| compare_end_order(a, b, id_map));

        self.active_by_id.clear();
        self.active_by_id_values.clear();
        self.start_cursor = 0;
        self.end_cursor = 0;
    }

    /// Rebuild the active set from scratch at the given beat.
    /// Unity ActiveTimelineClipWindow.RebuildActiveSetAtBeat (lines 147-184).
    fn rebuild_active_set_at_beat(&mut self, beat: f32) {
        self.active_by_id.clear();
        self.active_by_id_values.clear();

        let started_count = upper_bound_start(&self.clips_by_start, beat);
        let first_ending_after = lower_bound_end(&self.clips_by_end, beat);
        let ending_after_count = self.clips_by_end.len() - first_ending_after;

        self.start_cursor = started_count;
        self.end_cursor = first_ending_after;

        if started_count <= ending_after_count {
            for i in 0..started_count {
                let clip = &self.clips_by_start[i];
                if clip.end_beat() > beat {
                    self.active_by_id.insert(clip.id.clone(), clip.clone());
                    // PERF: keep parallel iteration list in sync (see field comment)
                    self.active_by_id_values.push(clip.clone());
                }
            }
            return;
        }

        for i in first_ending_after..self.clips_by_end.len() {
            let clip = &self.clips_by_end[i];
            if clip.start_beat <= beat {
                self.active_by_id.insert(clip.id.clone(), clip.clone());
                // PERF: keep parallel iteration list in sync (see field comment)
                self.active_by_id_values.push(clip.clone());
            }
        }
    }

    /// Incrementally advance the active set forward to `beat`.
    /// Unity ActiveTimelineClipWindow.AdvanceActiveSetForward (lines 186-222).
    fn advance_active_set_forward(&mut self, beat: f32) {
        // Walk start_cursor forward: add clips that have started
        while self.start_cursor < self.clips_by_start.len() {
            let clip = &self.clips_by_start[self.start_cursor];
            if clip.start_beat > beat {
                break;
            }
            if clip.end_beat() > beat {
                let clip_cloned = clip.clone();
                self.active_by_id.insert(clip_cloned.id.clone(), clip_cloned.clone());
                // PERF: keep parallel iteration list in sync (see field comment)
                self.active_by_id_values.push(clip_cloned);
            }
            self.start_cursor += 1;
        }

        // Walk end_cursor forward: remove clips that have ended
        while self.end_cursor < self.clips_by_end.len() {
            let clip_id;
            {
                let clip = &self.clips_by_end[self.end_cursor];
                if clip.end_beat() > beat {
                    break;
                }
                clip_id = clip.id.clone();
            }
            self.active_by_id.remove(&clip_id);
            // PERF: keep parallel iteration list in sync (see field comment)
            if let Some(pos) = self.active_by_id_values.iter().rposition(|c| c.id == clip_id) {
                self.active_by_id_values.remove(pos);
            }
            self.end_cursor += 1;
        }
    }

    /// Collect visible (non-muted, solo-respecting) clips into `results`.
    /// Unity ActiveTimelineClipWindow.CollectVisible (lines 224-249).
    fn collect_visible(&mut self, timeline: &Timeline, beat: f32, results: &mut Vec<TimelineClip>) {
        self.build_layer_visibility_cache(timeline);

        self.visible_scratch.clear();
        for clip in &self.active_by_id_values {
            if clip.is_muted {
                continue;
            }
            let layer_index = self.layer_id_to_index.get(&clip.layer_id).copied();
            let li = match layer_index {
                Some(idx) if idx < self.visible_layers.len() => idx,
                _ => continue,
            };
            if !self.visible_layers[li] {
                continue;
            }
            // Safety guard for timing edits while playback is running.
            if clip.start_beat <= beat && beat < clip.end_beat() {
                self.visible_scratch.push(clip.clone());
            }
        }

        let id_map = &self.layer_id_to_index;
        self.visible_scratch.sort_by(|a, b| compare_visible_order(a, b, id_map));
        results.extend_from_slice(&self.visible_scratch);
    }

    /// Build per-layer visibility flags respecting mute and solo.
    /// Unity ActiveTimelineClipWindow.BuildLayerVisibilityCache (lines 251-312).
    fn build_layer_visibility_cache(&mut self, timeline: &Timeline) {
        let layers = &timeline.layers;
        let layer_count = layers.len();

        if self.visible_layers.len() != layer_count {
            self.visible_layers.resize(layer_count, false);
        }

        // Check if any layer is solo'd
        let any_solo = layers.iter().any(|l| l.is_solo);

        // Build id → index map
        self.layer_id_to_index.clear();
        for (i, layer) in layers.iter().enumerate() {
            if !layer.layer_id.is_empty() {
                self.layer_id_to_index.insert(layer.layer_id.clone(), i);
            }
        }

        for i in 0..layer_count {
            let layer = &layers[i];
            let mut visible = !layer.is_group() && !layer.is_muted;

            if visible {
                if let Some(parent_id) = &layer.parent_layer_id {
                    // Child layer — check parent group state
                    let parent = self
                        .layer_id_to_index
                        .get(parent_id)
                        .copied()
                        .filter(|&pi| pi < layer_count)
                        .map(|pi| &layers[pi]);

                    if parent.is_some_and(|p| p.is_muted)
                        || (any_solo
                            && !layer.is_solo
                            && !parent.is_some_and(|p| p.is_solo))
                    {
                        visible = false;
                    }
                } else if any_solo && !layer.is_solo {
                    // Root layer solo check
                    visible = false;
                }
            }

            self.visible_layers[i] = visible;
        }
    }
}

impl Default for ActiveTimelineClipWindow {
    fn default() -> Self {
        Self::new()
    }
}

// ── Binary search helpers ────────────────────────────────────────────────────

/// Count of clips with start_beat <= beat (upper bound on start).
/// Unity ActiveTimelineClipWindow.UpperBoundStart (lines 320-334).
fn upper_bound_start(sorted_by_start: &[TimelineClip], beat: f32) -> usize {
    let mut lo = 0usize;
    let mut hi = sorted_by_start.len();
    while lo < hi {
        let mid = lo + ((hi - lo) >> 1);
        if sorted_by_start[mid].start_beat <= beat {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// First index where end_beat > beat (lower bound on end).
/// Unity ActiveTimelineClipWindow.LowerBoundEnd (lines 336-350).
fn lower_bound_end(sorted_by_end: &[TimelineClip], beat: f32) -> usize {
    let mut lo = 0usize;
    let mut hi = sorted_by_end.len();
    while lo < hi {
        let mid = lo + ((hi - lo) >> 1);
        if sorted_by_end[mid].end_beat() <= beat {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

// ── Comparators ─────────────────────────────────────────────────────────────

/// Sort order: StartBeat → EndBeat → LayerIndex → Id.
/// Unity ActiveTimelineClipWindow.CompareStartOrder (lines 352-365).
fn compare_start_order(a: &TimelineClip, b: &TimelineClip, id_map: &AHashMap<LayerId, usize>) -> std::cmp::Ordering {
    let ai = id_map.get(&a.layer_id).copied().unwrap_or(0);
    let bi = id_map.get(&b.layer_id).copied().unwrap_or(0);
    a.start_beat
        .partial_cmp(&b.start_beat)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| {
            a.end_beat()
                .partial_cmp(&b.end_beat())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| ai.cmp(&bi))
        .then_with(|| a.id.as_str().cmp(b.id.as_str()))
}

/// Sort order: EndBeat → StartBeat → LayerIndex → Id.
/// Unity ActiveTimelineClipWindow.CompareEndOrder (lines 367-380).
fn compare_end_order(a: &TimelineClip, b: &TimelineClip, id_map: &AHashMap<LayerId, usize>) -> std::cmp::Ordering {
    let ai = id_map.get(&a.layer_id).copied().unwrap_or(0);
    let bi = id_map.get(&b.layer_id).copied().unwrap_or(0);
    a.end_beat()
        .partial_cmp(&b.end_beat())
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| {
            a.start_beat
                .partial_cmp(&b.start_beat)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| ai.cmp(&bi))
        .then_with(|| a.id.as_str().cmp(b.id.as_str()))
}

/// Sort order: LayerIndex → StartBeat → EndBeat → Id.
/// Unity ActiveTimelineClipWindow.CompareVisibleOrder (lines 382-395).
fn compare_visible_order(a: &TimelineClip, b: &TimelineClip, id_map: &AHashMap<LayerId, usize>) -> std::cmp::Ordering {
    let ai = id_map.get(&a.layer_id).copied().unwrap_or(0);
    let bi = id_map.get(&b.layer_id).copied().unwrap_or(0);
    ai.cmp(&bi)
        .then_with(|| {
            a.start_beat
                .partial_cmp(&b.start_beat)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| {
            a.end_beat()
                .partial_cmp(&b.end_beat())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| a.id.as_str().cmp(b.id.as_str()))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::layer::Layer;
    use manifold_core::types::LayerType;

    // ── Test helpers ─────────────────────────────────────────────────────────

    fn make_clip(id: &str, _layer_index: i32, start_beat: f32, duration_beats: f32) -> TimelineClip {
        TimelineClip {
            id: ClipId::new(id),
            start_beat,
            duration_beats,
            ..Default::default()
        }
    }

    fn make_video_layer(index: i32, clips: Vec<TimelineClip>) -> Layer {
        let mut layer = Layer::new(format!("Layer {}", index), LayerType::Video, index);
        layer.clips = clips;
        layer
    }

    fn make_group_layer(index: i32) -> Layer {
        Layer::new(format!("Group {}", index), LayerType::Group, index)
    }

    fn make_project(layers: Vec<Layer>) -> Project {
        let mut project = Project::default();
        project.timeline.layers = layers;
        // Sync layer indices and clip layer_id values
        for (i, layer) in project.timeline.layers.iter_mut().enumerate() {
            layer.index = i as i32;
            for clip in &mut layer.clips {
                clip.layer_id = layer.layer_id.clone();
            }
        }
        project
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_basic_forward_advance() {
        // Clip A: beats 0-4, Clip B: beats 2-6.
        let clips = vec![
            make_clip("A", 0, 0.0, 4.0),
            make_clip("B", 0, 2.0, 4.0),
        ];
        let project = make_project(vec![make_video_layer(0, clips)]);
        let mut window = ActiveTimelineClipWindow::new();
        let mut results = Vec::new();

        // At beat 1.0: only A active
        window.get_active_clips(&project, 1.0, &mut results);
        let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"A"), "expected A at beat 1.0, got {:?}", ids);
        assert!(!ids.contains(&"B"), "expected no B at beat 1.0, got {:?}", ids);

        // At beat 3.0: A and B active (forward advance from 1.0)
        window.get_active_clips(&project, 3.0, &mut results);
        let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"A"), "expected A at beat 3.0, got {:?}", ids);
        assert!(ids.contains(&"B"), "expected B at beat 3.0, got {:?}", ids);

        // At beat 5.0: only B active (A ended at 4.0)
        window.get_active_clips(&project, 5.0, &mut results);
        let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
        assert!(!ids.contains(&"A"), "expected no A at beat 5.0, got {:?}", ids);
        assert!(ids.contains(&"B"), "expected B at beat 5.0, got {:?}", ids);

        // At beat 7.0: nothing active
        window.get_active_clips(&project, 7.0, &mut results);
        assert!(results.is_empty(), "expected empty at beat 7.0, got {:?}", results);
    }

    #[test]
    fn test_backward_epsilon_no_rebuild() {
        // Verify that a beat within BACKWARD_EPSILON of last_beat does not trigger a rebuild.
        let clips = vec![make_clip("A", 0, 0.0, 10.0)];
        let project = make_project(vec![make_video_layer(0, clips)]);
        let mut window = ActiveTimelineClipWindow::new();
        let mut results = Vec::new();

        window.get_active_clips(&project, 5.0, &mut results);
        assert_eq!(results.len(), 1);

        // Jitter within epsilon — must NOT trigger rebuild (clip still active)
        let jitter = 5.0 - (BACKWARD_EPSILON * 0.5);
        window.get_active_clips(&project, jitter, &mut results);
        assert_eq!(results.len(), 1, "clip should still be active after tiny backward jitter");
    }

    #[test]
    fn test_backward_seek_rebuilds() {
        // Going backward by more than epsilon must trigger rebuild.
        let clips = vec![
            make_clip("A", 0, 0.0, 4.0),
            make_clip("B", 0, 8.0, 4.0),
        ];
        let project = make_project(vec![make_video_layer(0, clips)]);
        let mut window = ActiveTimelineClipWindow::new();
        let mut results = Vec::new();

        // Advance to beat 9 (B is active)
        window.get_active_clips(&project, 9.0, &mut results);
        let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"B"));
        assert!(!ids.contains(&"A"));

        // Seek backward to beat 1 — must rebuild, A should be active, B not
        window.get_active_clips(&project, 1.0, &mut results);
        let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"A"), "expected A after backward seek, got {:?}", ids);
        assert!(!ids.contains(&"B"), "expected no B after backward seek, got {:?}", ids);
    }

    #[test]
    fn test_solo_layer_visibility() {
        // Layer 0 is normal, Layer 1 is solo. Only clips on layer 1 should appear.
        let clip_a = make_clip("A", 0, 0.0, 4.0);
        let clip_b = make_clip("B", 1, 0.0, 4.0);

        let layer0 = make_video_layer(0, vec![clip_a]);
        let mut layer1 = make_video_layer(1, vec![clip_b]);
        layer1.is_solo = true;

        let project = make_project(vec![layer0.clone(), layer1.clone()]);
        let mut window = ActiveTimelineClipWindow::new();
        let mut results = Vec::new();

        window.get_active_clips(&project, 2.0, &mut results);
        let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
        assert!(!ids.contains(&"A"), "non-solo layer clip A should be hidden, got {:?}", ids);
        assert!(ids.contains(&"B"), "solo layer clip B should be visible, got {:?}", ids);
    }

    #[test]
    fn test_muted_parent_hides_children() {
        // Group (layer 0, muted) → child layer (layer 1). Child clips must be hidden.
        let clip_child = make_clip("C", 1, 0.0, 4.0);

        let mut group = make_group_layer(0);
        group.is_muted = true;

        let child_layer_id = LayerId::new("child-layer-id");
        let mut child = make_video_layer(1, vec![clip_child]);
        child.layer_id = child_layer_id.clone();
        child.parent_layer_id = Some(group.layer_id.clone());

        let project = make_project(vec![group, child]);
        let mut window = ActiveTimelineClipWindow::new();
        let mut results = Vec::new();

        window.get_active_clips(&project, 2.0, &mut results);
        assert!(
            results.is_empty(),
            "child of muted group must be invisible, got {:?}",
            results
        );
    }

    #[test]
    fn test_sort_determinism() {
        // Three clips at the same start beat on the same layer — must sort by id.
        let clips = vec![
            make_clip("zzz", 0, 0.0, 4.0),
            make_clip("aaa", 0, 0.0, 4.0),
            make_clip("mmm", 0, 0.0, 4.0),
        ];
        let project = make_project(vec![make_video_layer(0, clips)]);
        let mut window = ActiveTimelineClipWindow::new();
        let mut results = Vec::new();

        window.get_active_clips(&project, 2.0, &mut results);
        assert_eq!(results.len(), 3);

        // Visible order: LayerIndex → StartBeat → EndBeat → Id (lexicographic)
        let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["aaa", "mmm", "zzz"], "clips must be sorted deterministically by id");
    }

    #[test]
    fn test_index_invalidation() {
        // Start with one clip, then add another — index must be rebuilt.
        let clip_a = make_clip("A", 0, 0.0, 4.0);
        let project_v1 = make_project(vec![make_video_layer(0, vec![clip_a])]);
        let mut window = ActiveTimelineClipWindow::new();
        let mut results = Vec::new();

        window.get_active_clips(&project_v1, 2.0, &mut results);
        assert_eq!(results.len(), 1);

        // Add a second clip — new project instance with two clips
        let clip_b = make_clip("B", 0, 1.0, 4.0);
        let clip_a2 = make_clip("A", 0, 0.0, 4.0);
        let project_v2 = make_project(vec![make_video_layer(0, vec![clip_a2, clip_b])]);

        window.get_active_clips(&project_v2, 2.0, &mut results);
        assert_eq!(results.len(), 2, "after adding a clip, both clips must be active");
        let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"A"));
        assert!(ids.contains(&"B"));
    }
}
