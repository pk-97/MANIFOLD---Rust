/// UI-specific state for timeline editor (session-only, not serialized).
/// Tracks selection, drag state, hover, and zoom.
///
/// Mechanical 1:1 port of Unity UIState.cs.
/// Replaces the former app::SelectionState + app::ClipDragState.

use std::collections::HashSet;
use manifold_core::selection::SelectionRegion;

pub struct UIState {
    // ── Clip Selection ──
    pub selected_clip_ids: HashSet<String>,

    /// Monotonically increasing counter — bumped on every selection/hover change.
    /// Used for cheap dirty-checking by viewport and layer headers.
    pub selection_version: u64,

    /// Most recently selected clip ID (for footer info, property display).
    pub primary_selected_clip_id: Option<String>,

    /// Layer index of the primary selected clip (-1 maps to None).
    pub selected_layer_index: Option<usize>,

    // ── Layer Selection (mutually exclusive with clip selection) ──
    pub selected_layer_ids: HashSet<String>,

    /// Most recently selected layer ID (for inspector display).
    pub primary_selected_layer_id: Option<String>,

    // ── Region Selection ──
    selection_region: SelectionRegion,

    // ── Cursor Position (updated on mouse move, used for paste target) ──
    pub cursor_beat: f32,
    pub cursor_layer_index: Option<usize>,

    // ── Insert Cursor (set on click, persists until next click or region drag) ──
    pub insert_cursor_beat: Option<f32>,
    pub insert_cursor_layer_index: Option<usize>,

    // ── Hover ──
    pub hovered_clip_id: Option<String>,

    // ── Drag state ──
    pub is_dragging: bool,
    pub drag_clip_id: Option<String>,
    pub drag_start_beat: f32,
    pub drag_start_layer: usize,
    pub drag_offset_beats: f32, // offset from clip StartBeat to mouse beat

    // ── Trim state (originals preserved for undo) ──
    pub is_trimming: bool,
    pub trim_from_left: bool, // true = left edge, false = right edge
    pub trim_clip_id: Option<String>,
    pub trim_original_start_beat: f32,
    pub trim_original_duration_beats: f32,
    pub trim_original_in_point: f32, // seconds (video source offset)

    // ── Scrubbing ──
    pub is_scrubbing: bool,

    // ── Zoom ──
    pub current_zoom_index: usize,
}

impl UIState {
    pub fn new() -> Self {
        Self {
            selected_clip_ids: HashSet::new(),
            selection_version: 0,
            primary_selected_clip_id: None,
            selected_layer_index: None,
            selected_layer_ids: HashSet::new(),
            primary_selected_layer_id: None,
            selection_region: SelectionRegion::default(),
            cursor_beat: 0.0,
            cursor_layer_index: None,
            insert_cursor_beat: None,
            insert_cursor_layer_index: None,
            hovered_clip_id: None,
            is_dragging: false,
            drag_clip_id: None,
            drag_start_beat: 0.0,
            drag_start_layer: 0,
            drag_offset_beats: 0.0,
            is_trimming: false,
            trim_from_left: false,
            trim_clip_id: None,
            trim_original_start_beat: 0.0,
            trim_original_duration_beats: 0.0,
            trim_original_in_point: 0.0,
            is_scrubbing: false,
            current_zoom_index: crate::color::DEFAULT_ZOOM_INDEX,
        }
    }

    // ── Clip Selection ──────────────────────────────────────────────

    /// Select a single clip (clears previous selection and region). Called on normal click.
    /// Unity UIState.cs SelectClip (lines 167-178).
    pub fn select_clip(&mut self, clip_id: String, layer_index: usize) {
        self.selection_region = SelectionRegion::default();
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_index = None;
        self.clear_layer_selection();
        self.selected_clip_ids.clear();
        self.selected_clip_ids.insert(clip_id.clone());
        self.primary_selected_clip_id = Some(clip_id);
        self.selected_layer_index = Some(layer_index);
        self.selection_version += 1;
    }

    /// Toggle a clip in/out of the selection set. Called on Ctrl+Click.
    /// Unity UIState.cs ToggleClipSelection (lines 183-208).
    pub fn toggle_clip_selection(&mut self, clip_id: String, layer_index: usize) {
        self.clear_layer_selection();
        if self.selected_clip_ids.contains(&clip_id) {
            self.selected_clip_ids.remove(&clip_id);
            if self.primary_selected_clip_id.as_ref() == Some(&clip_id) {
                // Pick a new primary or None
                self.primary_selected_clip_id = self.selected_clip_ids.iter().next().cloned();
                self.selected_layer_index = None;
            }
        } else {
            self.selected_clip_ids.insert(clip_id.clone());
            self.primary_selected_clip_id = Some(clip_id);
            self.selected_layer_index = Some(layer_index);
        }
        self.selection_version += 1;
    }

    /// Clear all selection (clips, layers, region, and insert cursor).
    /// Unity UIState.cs ClearSelection (lines 211-222).
    pub fn clear_selection(&mut self) {
        self.selected_clip_ids.clear();
        self.primary_selected_clip_id = None;
        self.selected_layer_index = None;
        self.selected_layer_ids.clear();
        self.primary_selected_layer_id = None;
        self.selection_region = SelectionRegion::default();
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_index = None;
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
    pub fn get_selected_clip_ids(&self) -> Vec<String> {
        self.selected_clip_ids.iter().cloned().collect()
    }

    /// Number of selected clips.
    pub fn selection_count(&self) -> usize {
        self.selected_clip_ids.len()
    }

    // ── Region Selection ────────────────────────────────────────────

    /// Set a region selection (clears individual clip and layer selection).
    /// Unity UIState.cs SetRegion (lines 50-68).
    pub fn set_region(&mut self, start_beat: f32, end_beat: f32, start_layer: i32, end_layer: i32) {
        self.selected_clip_ids.clear();
        self.primary_selected_clip_id = None;
        self.selected_layer_index = None;
        self.selected_layer_ids.clear();
        self.primary_selected_layer_id = None;
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_index = None;
        self.selection_region.set(start_beat, end_beat, start_layer, end_layer);
        self.selection_version += 1;
    }

    /// Set region from clip selection bounds. Unlike set_region(), this PRESERVES
    /// individual clip IDs so per-clip highlight and inspector still work.
    /// Unity UIState.cs SetRegionFromClipBounds (lines 74-92).
    pub fn set_region_from_clip_bounds(&mut self, start_beat: f32, end_beat: f32, start_layer: i32, end_layer: i32) {
        self.clear_layer_selection();
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_index = None;
        let s_layer = start_layer.min(end_layer);
        let e_layer = start_layer.max(end_layer);
        let s_beat = start_beat.min(end_beat);
        let e_beat = start_beat.max(end_beat);
        self.selection_region.set(s_beat, e_beat, s_layer, e_layer);
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

    /// Extend or create a region from the current anchor to the target beat/layer.
    /// This is the Shift+Click behavior from Unity InteractionOverlay.
    /// Anchor is determined from: existing region → insert cursor → primary clip → beat 0.
    pub fn select_region_to(&mut self, target_beat: f32, target_layer: usize) {
        // Determine anchor position
        let (anchor_beat, anchor_layer) = if self.selection_region.is_active {
            // Existing region: extend from the opposite end
            (self.selection_region.start_beat, self.selection_region.start_layer_index as usize)
        } else if let Some(beat) = self.insert_cursor_beat {
            // Insert cursor as anchor
            (beat, self.insert_cursor_layer_index.unwrap_or(0))
        } else {
            // No anchor — start from beat 0, layer 0
            (0.0, 0)
        };

        let min_beat = anchor_beat.min(target_beat);
        let max_beat = anchor_beat.max(target_beat);
        let min_layer = anchor_layer.min(target_layer) as i32;
        let max_layer = anchor_layer.max(target_layer) as i32;

        // Set region (clears clip/layer selection, sets region)
        self.set_region(min_beat, max_beat, min_layer, max_layer);
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
    pub fn set_insert_cursor(&mut self, beat: f32, layer_index: usize) {
        self.insert_cursor_beat = Some(beat);
        self.insert_cursor_layer_index = Some(layer_index);
        self.selection_region = SelectionRegion::default(); // cursor replaces region
        self.selected_clip_ids.clear(); // deselect clips (Ableton behavior)
        self.primary_selected_clip_id = None;
        self.selected_layer_index = None;
        self.selected_layer_ids.clear(); // deselect layer headers (Ableton behavior)
        self.primary_selected_layer_id = None;
        self.selection_version += 1;
    }

    /// Move the insert cursor beat without clearing selection (used during playhead scrub).
    /// Unity UIState.cs SetInsertCursorBeat (lines 125-130).
    pub fn set_insert_cursor_beat(&mut self, beat: f32) {
        self.insert_cursor_beat = Some(beat);
        if self.insert_cursor_layer_index.is_none() {
            self.insert_cursor_layer_index = Some(0);
        }
    }

    /// Clear insert cursor if active.
    /// Unity UIState.cs ClearInsertCursor (lines 132-138).
    pub fn clear_insert_cursor(&mut self) {
        if self.insert_cursor_layer_index.is_none() {
            return;
        }
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_index = None;
        self.selection_version += 1;
    }

    /// Whether an insert cursor is placed.
    pub fn has_insert_cursor(&self) -> bool {
        self.insert_cursor_layer_index.is_some()
    }

    // ── Layer Selection ─────────────────────────────────────────────

    /// Select a single layer (clears previous clip, layer, and region selection).
    /// Unity UIState.cs SelectLayer (lines 247-259).
    pub fn select_layer(&mut self, layer_id: String) {
        self.selection_region = SelectionRegion::default();
        self.selected_clip_ids.clear();
        self.primary_selected_clip_id = None;
        self.selected_layer_index = None;
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_index = None;
        self.selected_layer_ids.clear();
        self.selected_layer_ids.insert(layer_id.clone());
        self.primary_selected_layer_id = Some(layer_id);
        self.selection_version += 1;
    }

    /// Toggle a layer in/out of the selection set. Called on Cmd+Click.
    /// Unity UIState.cs ToggleLayerSelection (lines 264-291).
    pub fn toggle_layer_selection(&mut self, layer_id: String) {
        self.selected_clip_ids.clear();
        self.primary_selected_clip_id = None;
        self.selected_layer_index = None;
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_index = None;

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
    pub fn select_layer_range(&mut self, target_layer_id: &str, layers: &[manifold_core::layer::Layer]) {
        self.selected_clip_ids.clear();
        self.primary_selected_clip_id = None;
        self.selected_layer_index = None;
        self.insert_cursor_beat = None;
        self.insert_cursor_layer_index = None;

        let primary = match &self.primary_selected_layer_id {
            Some(id) => id.clone(),
            None => {
                self.select_layer(target_layer_id.to_string());
                return;
            }
        };

        let anchor_idx = layers.iter().position(|l| l.layer_id == primary);
        let target_idx = layers.iter().position(|l| l.layer_id == target_layer_id);

        match (anchor_idx, target_idx) {
            (Some(a), Some(t)) => {
                let lo = a.min(t);
                let hi = a.max(t);
                self.selected_layer_ids.clear();
                for i in lo..=hi {
                    self.selected_layer_ids.insert(layers[i].layer_id.clone());
                }
                // Keep primary as the anchor (first clicked), not the range end
                self.selection_version += 1;
            }
            _ => {
                self.select_layer(target_layer_id.to_string());
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
    pub fn is_layer_active(&self, layer_index: usize, layer_id: &str) -> bool {
        // 1. Explicit layer header selection
        if self.selected_layer_ids.contains(layer_id) {
            return true;
        }
        // 2. Clip selected on this layer
        if self.selected_layer_index == Some(layer_index) {
            return true;
        }
        // 3. Insert cursor on this layer
        if self.insert_cursor_layer_index == Some(layer_index) {
            return true;
        }
        // 4. Region selection spans this layer
        if self.selection_region.is_active {
            let li = layer_index as i32;
            if li >= self.selection_region.start_layer_index
                && li <= self.selection_region.end_layer_index
            {
                return true;
            }
        }
        false
    }

    /// Number of selected layers.
    pub fn layer_selection_count(&self) -> usize {
        self.selected_layer_ids.len()
    }

    // ── Drag ────────────────────────────────────────────────────────

    /// Begin a clip move drag.
    /// Unity UIState.cs BeginDrag (lines 374-381).
    pub fn begin_drag(&mut self, clip_id: &str, start_beat: f32, layer_index: usize, mouse_beat: f32) {
        self.is_dragging = true;
        self.drag_clip_id = Some(clip_id.to_string());
        self.drag_start_beat = start_beat;
        self.drag_start_layer = layer_index;
        self.drag_offset_beats = mouse_beat - start_beat;
    }

    /// End a clip move drag.
    /// Unity UIState.cs EndDrag (lines 383-387).
    pub fn end_drag(&mut self) {
        self.is_dragging = false;
        self.drag_clip_id = None;
    }

    // ── Trim ────────────────────────────────────────────────────────

    /// Begin a left-edge trim.
    /// Unity UIState.cs BeginTrimLeft (lines 389-397).
    pub fn begin_trim_left(&mut self, clip_id: &str, start_beat: f32, duration_beats: f32, in_point: f32) {
        self.is_trimming = true;
        self.trim_from_left = true;
        self.trim_clip_id = Some(clip_id.to_string());
        self.trim_original_start_beat = start_beat;
        self.trim_original_duration_beats = duration_beats;
        self.trim_original_in_point = in_point;
    }

    /// Begin a right-edge trim.
    /// Unity UIState.cs BeginTrimRight (lines 399-407).
    pub fn begin_trim_right(&mut self, clip_id: &str, start_beat: f32, duration_beats: f32, in_point: f32) {
        self.is_trimming = true;
        self.trim_from_left = false;
        self.trim_clip_id = Some(clip_id.to_string());
        self.trim_original_start_beat = start_beat;
        self.trim_original_duration_beats = duration_beats;
        self.trim_original_in_point = in_point;
    }

    /// End a trim operation.
    /// Unity UIState.cs EndTrim (lines 409-413).
    pub fn end_trim(&mut self) {
        self.is_trimming = false;
        self.trim_clip_id = None;
    }
}
