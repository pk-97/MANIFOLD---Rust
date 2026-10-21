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

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(id: &str, layer: usize, start: f32, dur: f32) -> ViewportClip {
        ViewportClip {
            clip_id: id.into(),
            layer_index: layer,
            start_beat: Beats::from_f32(start),
            duration_beats: Beats::from_f32(dur),
            name: id.into(),
            color: color::CLIP_NORMAL,
            is_muted: false,
            is_locked: false,
            is_generator: false,
            is_audio: false,
            waveform: None,
            in_point_seconds: 0.0,
            waveform_breakpoints: Vec::new(),
        }
    }

    fn panel() -> TimelineViewportPanel {
        let mut panel = TimelineViewportPanel::new();
        panel.set_tracks(vec![TrackInfo::default(), TrackInfo::default()]);
        panel
    }

    /// The overview normalises by the last clip end. A scroll or zoom reads
    /// the value held since the clip data last moved; it always equals a full
    /// walk.
    #[test]
    fn overview_max_beat_matches_a_full_walk() {
        let mut panel = panel();
        panel.set_clips(vec![clip("a", 0, 0.0, 4.0), clip("b", 1, 4.0, 8.0)]);
        panel.repaint_overview();
        assert_eq!(panel.overview_max_beat, 12.0);
        assert_eq!(panel.overview_max_beat, panel.clips_end_beat());

        // A scroll and a zoom leave the clip data alone and read the held value.
        panel.set_scroll(8.0, 0.0);
        panel.repaint_overview();
        panel.set_zoom(40.0);
        panel.repaint_overview();
        assert_eq!(panel.overview_max_beat, 12.0);

        // A preview move that extends the end is seen on the next repaint.
        let id: ClipId = "a".into();
        panel.preview_clip_mut(&id).unwrap().start_beat = Beats::from_f32(30.0);
        panel.repaint_overview();
        assert_eq!(panel.overview_max_beat, 34.0);
        assert_eq!(panel.overview_max_beat, panel.clips_end_beat());

        // A move to another layer keeps the end; set_clips again shrinks it.
        assert!(panel.move_clip_to_layer(&id, 1));
        panel.repaint_overview();
        assert_eq!(panel.overview_max_beat, 34.0);
        panel.set_clips(vec![clip("b", 1, 4.0, 8.0)]);
        panel.repaint_overview();
        assert_eq!(panel.overview_max_beat, 12.0);
    }
}
