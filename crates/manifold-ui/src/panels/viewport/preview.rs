//! In-place clip writes for a drag preview (UI_INVALIDATION_DESIGN.md P7a).
//!
//! A preview move touches only the clips the gesture drags. The panel keeps a
//! clip-id index (built by `set_clips`) so the app writes those entries
//! directly instead of re-projecting every clip in the project. The index is
//! the only addition; the clips stay in `clips_by_layer`.

use super::*;

impl TimelineViewportPanel {
    /// Clips the panel holds. A project clip count that differs means a clip
    /// was added or removed (a region-move split), which this path cannot show.
    pub fn clip_total(&self) -> usize {
        self.clip_index.len()
    }

    /// The entry for one clip, read-only. `None` when the clip is not shown.
    pub fn preview_clip(&self, clip_id: &ClipId) -> Option<&ViewportClip> {
        let &(layer, pos) = self.clip_index.get(clip_id)?;
        self.clips_by_layer.get(layer)?.get(pos)
    }

    /// The entry for one clip, marked for the overview's clip layer to repaint.
    /// `None` when the clip is not shown.
    pub fn preview_clip_mut(&mut self, clip_id: &ClipId) -> Option<&mut ViewportClip> {
        let &(layer, pos) = self.clip_index.get(clip_id)?;
        self.overview_clips_dirty = true;
        self.overview_repaint = true;
        self.clips_by_layer.get_mut(layer)?.get_mut(pos)
    }

    /// Move a clip's entry to another layer's bucket, keeping the index right.
    /// Returns false when the clip or the layer is not there.
    pub fn move_clip_to_layer(&mut self, clip_id: &ClipId, layer_index: usize) -> bool {
        let Some(&(layer, pos)) = self.clip_index.get(clip_id) else {
            return false;
        };
        if layer == layer_index {
            return true;
        }
        if layer_index >= self.clips_by_layer.len() || layer >= self.clips_by_layer.len() {
            return false;
        }
        // `swap_remove` keeps the move O(1); the clip that takes the hole
        // gets its index entry rewritten. Clips on a layer never overlap, so
        // bucket order carries no meaning.
        let mut clip = self.clips_by_layer[layer].swap_remove(pos);
        if let Some(swapped) = self.clips_by_layer[layer].get(pos)
            && let Some(slot) = self.clip_index.get_mut(&swapped.clip_id)
        {
            slot.1 = pos;
        }
        clip.layer_index = layer_index;
        let dst = &mut self.clips_by_layer[layer_index];
        self.clip_index.insert(clip_id.clone(), (layer_index, dst.len()));
        dst.push(clip);
        self.overview_clips_dirty = true;
        self.overview_repaint = true;
        true
    }
}
