// UI-specific state for timeline editor (session-only, not serialized).
// Tracks selection, drag state, hover, and zoom.
//
// Mechanical 1:1 port of Unity UIState.cs.
// Replaces the former app::SelectionState + app::ClipDragState.

use crate::view::SelectionRegion;
use manifold_foundation::{Beats, ClipId, LayerId, MarkerId};
use std::collections::HashSet;

pub struct UIState {
    // ── Clip Selection ──
    pub selected_clip_ids: HashSet<ClipId>,

    /// Monotonically increasing counter — bumped on every selection/hover change.
    /// Used for cheap dirty-checking by viewport and layer headers.
    pub selection_version: u64,

    /// Most recently selected clip ID (for footer info, property display).
    pub primary_selected_clip_id: Option<ClipId>,

    /// LayerId of the layer the primary selected clip is on.
    pub selected_layer_id_for_clip: Option<LayerId>,

    // ── Layer Selection (mutually exclusive with clip selection) ──
    pub selected_layer_ids: HashSet<LayerId>,

    /// Most recently selected layer ID (for inspector display).
    pub primary_selected_layer_id: Option<LayerId>,

    // ── Region Selection ──
    selection_region: SelectionRegion,

    // ── Cursor Position (updated on mouse move, used for paste target) ──
    pub cursor_beat: f32,
    pub cursor_layer_id: Option<LayerId>,

    // ── Insert Cursor (set on click, persists until next click or region drag) ──
    pub insert_cursor_beat: Option<Beats>,
    pub insert_cursor_layer_id: Option<LayerId>,

    // ── Hover ──
    pub hovered_clip_id: Option<ClipId>,

    // Transient drag/trim/scrub state is NOT here — it has a single owner,
    // `InteractionOverlay` (the timeline interaction component). UIState holds
    // only the persistent selection/cursor/zoom the renderer reads. See
    // `docs/TIMELINE_API_DESIGN.md` §3.3.

    // ── Zoom ──
    pub current_zoom_index: usize,

    // ── Marker Selection ──
    pub selected_marker_ids: HashSet<MarkerId>,

    // ── Inspector scope ──
    /// The `selection_version` at which the inspector was pinned to the Master
    /// scope (by clicking the Master tab). Master is active iff this still
    /// equals the current `selection_version` — so any selection change, which
    /// already bumps the version, auto-clears the pin and pulls the inspector
    /// back to the selected thing. The timeline selection itself is preserved.
    /// This is the one piece of inspector state that isn't pure selection — it
    /// lets Master be reached without a fake timeline lane or losing your
    /// place. See docs/UI_LAYOUT_DESIGN.md.
    master_pinned_at_version: Option<u64>,
}

impl Default for UIState {
    fn default() -> Self {
        Self::new()
    }
}

impl UIState {
    pub fn new() -> Self {
        Self {
            selected_clip_ids: HashSet::new(),
            selection_version: 0,
            primary_selected_clip_id: None,
            selected_layer_id_for_clip: None,
            selected_layer_ids: HashSet::new(),
            primary_selected_layer_id: None,
            selection_region: SelectionRegion::default(),
            cursor_beat: 0.0,
            cursor_layer_id: None,
            insert_cursor_beat: None,
            insert_cursor_layer_id: None,
            hovered_clip_id: None,
            current_zoom_index: crate::color::DEFAULT_ZOOM_INDEX,
            selected_marker_ids: HashSet::new(),
            master_pinned_at_version: None,
        }
    }

    // ── Inspector scope ─────────────────────────────────────────────

    /// Whether the inspector is currently pinned to the Master scope. True only
    /// while no selection change has happened since the Master tab was clicked.
    pub fn master_scope_active(&self) -> bool {
        self.master_pinned_at_version == Some(self.selection_version)
    }

    /// Pin the inspector to the Master scope without touching the timeline
    /// selection. Bumps `selection_version` so the inspector rebuilds; the pin
    /// is recorded against the new version and auto-clears on the next
    /// selection change (which bumps the version again).
    pub fn select_master_scope(&mut self) {
        if self.master_scope_active() {
            return;
        }
        self.selection_version += 1;
        self.master_pinned_at_version = Some(self.selection_version);
    }

    /// Release the Master pin without changing the timeline selection, so the
    /// inspector falls back to the selected scope. Bumps `selection_version`
    /// (and so triggers a rebuild) only if a pin was actually set.
    pub fn clear_master_scope(&mut self) {
        if self.master_pinned_at_version.take().is_some() {
            self.selection_version += 1;
        }
    }

    // ── Clip Selection ──────────────────────────────────────────────

    /// Select a single clip (clears previous selection and region). Called on normal click.
    /// Unity UIState.cs SelectClip (lines 167-178).
    pub fn select_clip(&mut self, clip_id: ClipId, layer_id: LayerId) {
        self.selection_region = SelectionRegion::default();
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        self.clear_layer_selection();
        self.selected_clip_ids.clear();
        self.selected_clip_ids.insert(clip_id.clone());
        self.primary_selected_clip_id = Some(clip_id);
        self.selected_layer_id_for_clip = Some(layer_id);
        self.selection_version += 1;
    }

    /// Toggle a clip in/out of the selection set. Called on Ctrl+Click.
    /// Unity UIState.cs ToggleClipSelection (lines 183-208).
    pub fn toggle_clip_selection(&mut self, clip_id: ClipId, layer_id: LayerId) {
        self.clear_layer_selection();
        if self.selected_clip_ids.contains(&clip_id) {
            self.selected_clip_ids.remove(&clip_id);
            if self.primary_selected_clip_id.as_ref() == Some(&clip_id) {
                // Pick a new primary or None
                self.primary_selected_clip_id = self.selected_clip_ids.iter().next().cloned();
                self.selected_layer_id_for_clip = None;
            }
        } else {
            self.selected_clip_ids.insert(clip_id.clone());
            self.primary_selected_clip_id = Some(clip_id);
            self.selected_layer_id_for_clip = Some(layer_id);
        }
        self.selection_version += 1;
    }

    /// Clear all selection (clips, layers, markers, region, and insert cursor).
    /// Unity UIState.cs ClearSelection (lines 211-222).
    pub fn clear_selection(&mut self) {
        self.selected_clip_ids.clear();
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.selected_layer_ids.clear();
        self.primary_selected_layer_id = None;
        self.selected_marker_ids.clear();
        self.selection_region = SelectionRegion::default();
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        self.selection_version += 1;
    }

    /// Check if a clip is in the selection set.
    pub fn is_selected(&self, clip_id: &str) -> bool {
        self.selected_clip_ids.contains(clip_id)
    }

    /// Check if a clip is hovered.
    pub fn is_hovered(&self, clip_id: &str) -> bool {
        self.hovered_clip_id.as_deref() == Some(clip_id)
    }

    /// Get a copy of all selected clip IDs.
    pub fn get_selected_clip_ids(&self) -> Vec<ClipId> {
        self.selected_clip_ids.iter().cloned().collect()
    }

    /// Number of selected clips.
    pub fn selection_count(&self) -> usize {
        self.selected_clip_ids.len()
    }

    // ── Region Selection ────────────────────────────────────────────

    /// Set a region selection (clears individual clip and layer selection).
    /// Unity UIState.cs SetRegion (lines 50-68).
    pub fn set_region(
        &mut self,
        start_beat: Beats,
        end_beat: Beats,
        start_layer: i32,
        end_layer: i32,
        layers: &[crate::view::UiLayer],
    ) {
        self.selected_clip_ids.clear();
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.selected_layer_ids.clear();
        self.primary_selected_layer_id = None;
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        // Populate LayerId-based fields from the layer array
        let min = start_layer.min(end_layer).max(0) as usize;
        let max = start_layer.max(end_layer).max(0) as usize;
        self.selection_region.start_beat = start_beat;
        self.selection_region.end_beat = end_beat;
        self.selection_region.is_active = true;
        self.selection_region.selected_layer_ids.clear();
        let upper = max.min(layers.len().saturating_sub(1));
        for layer in &layers[min..=upper] {
            self.selection_region
                .selected_layer_ids
                .insert(layer.layer_id.clone());
        }
        self.selection_region.start_layer_id = layers.get(min).map(|l| l.layer_id.clone());
        self.selection_region.end_layer_id = layers.get(max).map(|l| l.layer_id.clone());
        self.selection_version += 1;
    }

    /// Set region from clip selection bounds. Unlike set_region(), this PRESERVES
    /// individual clip IDs so per-clip highlight and inspector still work.
    /// Unity UIState.cs SetRegionFromClipBounds (lines 74-92).
    pub fn set_region_from_clip_bounds(
        &mut self,
        start_beat: Beats,
        end_beat: Beats,
        start_layer: i32,
        end_layer: i32,
        layers: &[crate::view::UiLayer],
    ) {
        self.clear_layer_selection();
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        let s_layer = start_layer.min(end_layer).max(0) as usize;
        let e_layer = start_layer.max(end_layer).max(0) as usize;
        let s_beat = start_beat.min(end_beat);
        let e_beat = start_beat.max(end_beat);
        self.selection_region.start_beat = s_beat;
        self.selection_region.end_beat = e_beat;
        self.selection_region.is_active = true;
        self.selection_region.selected_layer_ids.clear();
        let upper = e_layer.min(layers.len().saturating_sub(1));
        for layer in &layers[s_layer..=upper] {
            self.selection_region
                .selected_layer_ids
                .insert(layer.layer_id.clone());
        }
        self.selection_region.start_layer_id = layers.get(s_layer).map(|l| l.layer_id.clone());
        self.selection_region.end_layer_id = layers.get(e_layer).map(|l| l.layer_id.clone());
        self.selection_version += 1;
    }

    /// Clear the region selection (does NOT clear individual clip selection).
    /// Unity UIState.cs ClearRegion (lines 95-100).
    pub fn clear_region(&mut self) {
        if !self.selection_region.is_active {
            return;
        }
        self.selection_region.clear();
        self.selection_version += 1;
    }

    /// Whether a region selection is active.
    pub fn has_region(&self) -> bool {
        self.selection_region.is_active
    }

    /// Get the current selection region.
    pub fn get_region(&self) -> &SelectionRegion {
        &self.selection_region
    }

    // ── Insert Cursor ───────────────────────────────────────────────

    /// Set insert cursor. Clears EVERYTHING (clips, layers, region) per Ableton behavior.
    /// Unity UIState.cs SetInsertCursor (lines 111-122).
    pub fn set_insert_cursor(&mut self, beat: Beats, layer_id: LayerId) {
        // Skip if nothing would change — same position, no active selection to clear.
        if self.insert_cursor_beat == Some(beat)
            && self.insert_cursor_layer_id.as_ref() == Some(&layer_id)
            && self.selected_clip_ids.is_empty()
            && self.selected_layer_ids.is_empty()
            && !self.selection_region.is_active
        {
            return;
        }
        self.insert_cursor_beat = Some(beat);
        self.insert_cursor_layer_id = Some(layer_id);
        self.selection_region = SelectionRegion::default(); // cursor replaces region
        self.selected_clip_ids.clear(); // deselect clips (Ableton behavior)
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.selected_layer_ids.clear(); // deselect layer headers (Ableton behavior)
        self.primary_selected_layer_id = None;
        self.selection_version += 1;
    }

    /// Move the insert cursor beat without clearing selection (used during playhead scrub).
    /// Unity UIState.cs SetInsertCursorBeat (lines 125-130).
    pub fn set_insert_cursor_beat(&mut self, beat: Beats) {
        self.insert_cursor_beat = Some(beat);
    }

    /// Clear insert cursor if active.
    /// Unity UIState.cs ClearInsertCursor (lines 132-138).
    pub fn clear_insert_cursor(&mut self) {
        if self.insert_cursor_layer_id.is_none() {
            return;
        }
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        self.selection_version += 1;
    }

    /// Whether an insert cursor is placed.
    pub fn has_insert_cursor(&self) -> bool {
        self.insert_cursor_layer_id.is_some()
    }

    // ── Layer Selection ─────────────────────────────────────────────

    /// Select a single layer (clears previous clip, layer, and region selection).
    /// Unity UIState.cs SelectLayer (lines 247-259).
    pub fn select_layer(&mut self, layer_id: LayerId) {
        self.selection_region = SelectionRegion::default();
        self.selected_clip_ids.clear();
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        self.selected_layer_ids.clear();
        self.selected_layer_ids.insert(layer_id.clone());
        self.primary_selected_layer_id = Some(layer_id);
        self.selection_version += 1;
    }

    /// Toggle a layer in/out of the selection set. Called on Cmd+Click.
    /// Unity UIState.cs ToggleLayerSelection (lines 264-291).
    pub fn toggle_layer_selection(&mut self, layer_id: LayerId) {
        self.selected_clip_ids.clear();
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;

        if self.selected_layer_ids.contains(&layer_id) {
            self.selected_layer_ids.remove(&layer_id);
            if self.primary_selected_layer_id.as_ref() == Some(&layer_id) {
                self.primary_selected_layer_id = self.selected_layer_ids.iter().next().cloned();
            }
        } else {
            self.selected_layer_ids.insert(layer_id.clone());
            self.primary_selected_layer_id = Some(layer_id);
        }
        self.selection_version += 1;
    }

    /// Select a range of layers from primary to target (Shift+Click).
    /// Unity UIState.cs SelectLayerRange (lines 297-333).
    pub fn select_layer_range(
        &mut self,
        target_layer_id: &str,
        layers: &[crate::view::UiLayer],
    ) {
        self.selected_clip_ids.clear();
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;

        let primary = match &self.primary_selected_layer_id {
            Some(id) => id.clone(),
            None => {
                self.select_layer(LayerId::new(target_layer_id));
                return;
            }
        };

        let anchor_idx = layers.iter().position(|l| l.layer_id == *primary);
        let target_idx = layers.iter().position(|l| l.layer_id == target_layer_id);

        match (anchor_idx, target_idx) {
            (Some(a), Some(t)) => {
                let lo = a.min(t);
                let hi = a.max(t);
                self.selected_layer_ids.clear();
                for layer in &layers[lo..=hi] {
                    self.selected_layer_ids.insert(layer.layer_id.clone());
                }
                // Keep primary as the anchor (first clicked), not the range end
                self.selection_version += 1;
            }
            _ => {
                self.select_layer(LayerId::new(target_layer_id));
            }
        }
    }

    /// Clear layer selection only (preserves clip selection).
    /// Unity UIState.cs ClearLayerSelection (lines 336-340).
    pub fn clear_layer_selection(&mut self) {
        self.selected_layer_ids.clear();
        self.primary_selected_layer_id = None;
    }

    /// Check if a layer is explicitly selected (layer selection only).
    pub fn is_layer_selected(&self, layer_id: &str) -> bool {
        self.selected_layer_ids.contains(layer_id)
    }

    /// Unified check: is this layer "active" for ANY reason?
    /// Covers all interaction paths: explicit layer selection, clip selection,
    /// insert cursor placement, and region selection.
    /// Unity UIState.cs IsLayerActive (lines 353-366).
    pub fn is_layer_active(&self, layer_id: &LayerId) -> bool {
        // 1. Explicit layer header selection
        if self.selected_layer_ids.contains(layer_id) {
            return true;
        }
        // 2. Clip selected on this layer
        if self.selected_layer_id_for_clip.as_ref() == Some(layer_id) {
            return true;
        }
        // 3. Insert cursor on this layer
        if self.insert_cursor_layer_id.as_ref() == Some(layer_id) {
            return true;
        }
        // 4. Region selection spans this layer
        if self.selection_region.is_active && self.selection_region.contains_layer_id(layer_id) {
            return true;
        }
        false
    }

    /// Number of selected layers.
    pub fn layer_selection_count(&self) -> usize {
        self.selected_layer_ids.len()
    }

    // Drag/trim lifecycle (begin/end) lives on InteractionOverlay — the single
    // owner of transient gesture state. UIState used to mirror it here.

    // ── Selection Validation ───────────────────────────────────────

    /// Remove clip/layer IDs from selection that no longer exist in the project.
    /// Called after accepting a new project snapshot with a changed data_version.
    /// Returns true if any references were pruned.
    pub fn prune_stale_references(
        &mut self,
        valid_clip_ids: &HashSet<ClipId>,
        valid_layer_ids: &HashSet<LayerId>,
    ) -> bool {
        let mut changed = false;

        // Prune clip IDs
        let before = self.selected_clip_ids.len();
        self.selected_clip_ids
            .retain(|id| valid_clip_ids.contains(id));
        if self.selected_clip_ids.len() != before {
            changed = true;
        }

        if let Some(ref id) = self.primary_selected_clip_id
            && !valid_clip_ids.contains(id)
        {
            self.primary_selected_clip_id = None;
            self.selected_layer_id_for_clip = None;
            changed = true;
        }

        if let Some(ref id) = self.hovered_clip_id
            && !valid_clip_ids.contains(id)
        {
            self.hovered_clip_id = None;
            changed = true;
        }

        // Prune layer IDs
        let before = self.selected_layer_ids.len();
        self.selected_layer_ids
            .retain(|id| valid_layer_ids.contains(id));
        if self.selected_layer_ids.len() != before {
            changed = true;
        }

        if let Some(ref id) = self.primary_selected_layer_id
            && !valid_layer_ids.contains(id)
        {
            self.primary_selected_layer_id = None;
            changed = true;
        }

        if let Some(ref id) = self.selected_layer_id_for_clip
            && !valid_layer_ids.contains(id)
        {
            self.selected_layer_id_for_clip = None;
            changed = true;
        }

        if let Some(ref id) = self.insert_cursor_layer_id
            && !valid_layer_ids.contains(id)
        {
            self.insert_cursor_layer_id = None;
            changed = true;
        }

        // Prune region layer IDs
        if self.selection_region.is_active {
            let before = self.selection_region.selected_layer_ids.len();
            self.selection_region
                .selected_layer_ids
                .retain(|id| valid_layer_ids.contains(id));
            if self.selection_region.selected_layer_ids.len() != before {
                changed = true;
            }
            if self.selection_region.selected_layer_ids.is_empty() {
                self.selection_region.clear();
                changed = true;
            }
        }

        if changed {
            self.selection_version += 1;
        }
        changed
    }

    // ── Marker Selection ────────────────────────────────────────────

    /// Select a single marker (clears clips, layers, region, cursor).
    pub fn select_marker(&mut self, marker_id: MarkerId) {
        self.selected_clip_ids.clear();
        self.primary_selected_clip_id = None;
        self.selected_layer_id_for_clip = None;
        self.selected_layer_ids.clear();
        self.primary_selected_layer_id = None;
        self.selection_region = SelectionRegion::default();
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_id = None;
        self.selected_marker_ids.clear();
        self.selected_marker_ids.insert(marker_id);
        self.selection_version += 1;
    }

    /// Toggle a marker in/out of multi-selection (Shift+Click).
    pub fn toggle_marker_selection(&mut self, marker_id: MarkerId) {
        if self.selected_marker_ids.contains(&marker_id) {
            self.selected_marker_ids.remove(&marker_id);
        } else {
            self.selected_marker_ids.insert(marker_id);
        }
        self.selection_version += 1;
    }

    /// Whether a specific marker is selected.
    pub fn is_marker_selected(&self, marker_id: &MarkerId) -> bool {
        self.selected_marker_ids.contains(marker_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_pin_sets_and_self_clears_on_selection() {
        let mut s = UIState::new();
        assert!(!s.master_scope_active());

        // Pinning Master activates it and preserves selection.
        s.select_master_scope();
        assert!(s.master_scope_active());

        // Any selection change auto-clears the pin (version moves on).
        s.select_layer(LayerId::new("layer-a"));
        assert!(!s.master_scope_active());
        // ...and the layer is genuinely selected (pin didn't disturb it).
        assert_eq!(s.primary_selected_layer_id.as_deref(), Some("layer-a"));
    }

    #[test]
    fn clear_master_scope_releases_pin_without_touching_selection() {
        let mut s = UIState::new();
        s.select_clip(ClipId::new("clip-1"), LayerId::new("layer-a"));
        s.select_master_scope();
        assert!(s.master_scope_active());

        s.clear_master_scope();
        assert!(!s.master_scope_active());
        // The clip selection is intact — clearing the pin only changes scope.
        assert_eq!(s.primary_selected_clip_id.as_deref(), Some("clip-1"));
    }

    #[test]
    fn pinning_master_twice_is_idempotent() {
        let mut s = UIState::new();
        s.select_master_scope();
        let v = s.selection_version;
        s.select_master_scope(); // already pinned — no extra version churn
        assert_eq!(s.selection_version, v);
        assert!(s.master_scope_active());
    }
}
