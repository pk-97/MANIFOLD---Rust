//! Callback interface for UI-side effects that InputHandler cannot own.
//! Mechanical translation of Assets/Scripts/UI/Timeline/ITimelineInputHost.cs.
//!
//! Implemented by the app layer (WorkspaceController equivalent) as thin delegations.
//! InputHandler calls through this trait for operations that need engine/UI access.

/// Callback interface for InputHandler → host communication.
/// Port of ITimelineInputHost.cs — every method maps 1:1.
pub trait TimelineInputHost {
    /// Handle inspector-specific keyboard input (e.g., arrow key stepping for loop duration).
    /// Returns true if the key was consumed by the inspector.
    fn handle_inspector_keyboard(&mut self) -> bool;

    /// Toggle the performance HUD visibility.
    fn toggle_performance_hud(&mut self);

    /// Whether the monitor output window is currently active.
    fn is_monitor_output_active(&self) -> bool;

    /// Request a full UI rebuild (structural change).
    fn request_rebuild(&mut self);

    /// Called after undo/redo to refresh UI state.
    fn on_undo_redo(&mut self);

    /// Called when selection is cleared (Escape key).
    fn on_selection_cleared(&mut self);

    /// Mark the compositor as dirty (needs re-render).
    fn mark_compositor_dirty(&mut self);

    /// Invalidate all layer bitmaps (forces full repaint).
    fn invalidate_all_layer_bitmaps(&mut self);

    /// Update the zoom label display in the header.
    fn update_zoom_label(&mut self);

    /// Get the playhead's X position in viewport-local pixels.
    fn get_playhead_viewport_x(&self) -> f32;

    /// Get the viewport width in pixels.
    fn get_viewport_width(&self) -> f32;

    /// Get the current seconds per beat.
    fn get_seconds_per_beat(&self) -> f32;

    /// Notify host that a clip was selected (updates inspector).
    fn on_clip_selected(&mut self, clip_id: &str);

    // ── Effect keyboard shortcuts (context-sensitive routing) ────

    /// Copy selected effects. Returns true if handled.
    fn handle_effect_copy(&mut self) -> bool;

    /// Cut selected effects. Returns true if handled.
    fn handle_effect_cut(&mut self) -> bool;

    /// Paste effects. Returns true if handled.
    fn handle_effect_paste(&mut self) -> bool;

    /// Delete selected effects. Returns true if handled.
    fn handle_effect_delete(&mut self) -> bool;

    /// Group selected effects. Returns true if handled.
    fn handle_effect_group(&mut self) -> bool;

    /// Ungroup selected effects. Returns true if handled.
    fn handle_effect_ungroup(&mut self) -> bool;

    /// Clear effect selection in the inspector.
    fn clear_effect_selection(&mut self);

    /// Set inspector focus state.
    fn set_inspector_focus(&mut self, focused: bool);

    // ── Toast feedback ──────────────────────────────────────────

    /// Show a toast notification.
    fn show_toast(&mut self, message: &str);

    // ── Additional methods needed by InputHandler ───────────────
    // These are called directly on other services in Unity but need
    // to go through the host in Rust to avoid dependency issues.

    /// Undo the last command.
    fn undo(&mut self);

    /// Redo the last undone command.
    fn redo(&mut self);

    /// Save the current project.
    fn save_project(&mut self);

    /// Open a project file.
    fn open_project(&mut self);

    /// Create a new empty project.
    fn new_project(&mut self);

    /// Play or pause playback. If paused and insert cursor exists, seek to cursor first.
    fn play_pause(&mut self, insert_cursor_beat: Option<f32>);

    /// Seek to a specific time in seconds.
    fn seek_to(&mut self, time: f32);

    /// Get the current playback beat.
    fn current_beat(&self) -> f32;

    /// Whether playback is active.
    fn is_playing(&self) -> bool;

    /// Select all clips across all layers.
    fn select_all_clips(&mut self);

    /// Copy selected clips to clipboard.
    fn copy_clips(&mut self, clip_ids: &[String]);

    /// Cut selected clips (copy + delete).
    fn cut_clips(&mut self, clip_ids: &[String], has_region: bool);

    /// Paste clips at target position.
    fn paste_clips(&mut self, target_beat: f32, target_layer: i32);

    /// Duplicate selected clips.
    fn duplicate_clips(&mut self, clip_ids: &[String]);

    /// Delete selected clips (region-aware).
    fn delete_clips(&mut self, clip_ids: &[String], has_region: bool);

    /// Delete a layer by index.
    fn delete_layer(&mut self, layer_index: usize);

    /// Split selected clips at the current playhead beat.
    fn split_clips_at_playhead(&mut self, clip_ids: &[String]);

    /// Extend selected clips by grid step.
    fn extend_clips(&mut self, clip_ids: &[String], grid_step: f32);

    /// Shrink selected clips by grid step.
    fn shrink_clips(&mut self, clip_ids: &[String], grid_step: f32);

    /// Nudge selected clips by beat delta.
    fn nudge_clips(&mut self, clip_ids: &[String], beat_delta: f32);

    /// Toggle mute on selected clips.
    fn toggle_mute_clips(&mut self, clip_ids: &[String]);

    /// Group selected layers.
    fn group_selected_layers(&mut self);

    /// Delete selected layers.
    fn delete_selected_layers(&mut self);

    /// Number of layers in the project.
    fn layer_count(&self) -> usize;

    /// Get the project's beats per bar (time signature numerator).
    fn project_beats_per_bar(&self) -> u32;

    /// Set export in point at the current playhead beat.
    fn set_export_in_at_playhead(&mut self);

    /// Set export out point at the current playhead beat.
    fn set_export_out_at_playhead(&mut self);

    /// Clear the export in point.
    fn clear_export_in(&mut self);

    /// Clear the export out point.
    fn clear_export_out(&mut self);

    /// Dismiss any open context menu / dropdown.
    fn dismiss_context_menu(&mut self);

    /// Whether a context menu is currently open.
    fn has_context_menu(&self) -> bool;

    /// Get the current grid step in beats (from viewport zoom level).
    fn grid_step(&self) -> f32;

    /// Navigate the insert cursor (arrow keys). Direction: 0=left, 1=right, 2=up, 3=down.
    /// `is_fine`: true when Shift is held (1/16 beat step).
    /// Returns true if the navigation resulted in a clip auto-select.
    fn navigate_cursor(&mut self, direction: u8, is_fine: bool, grid_step: f32);

    // ── UIState delegation (InputHandler reads selection through host) ──

    /// Get IDs of all selected clips.
    fn get_selected_clip_ids(&self) -> Vec<String>;

    /// Number of selected clips.
    fn selection_count(&self) -> usize;

    /// Number of selected layers.
    fn layer_selection_count(&self) -> usize;

    /// Whether a selection region is active.
    fn has_region(&self) -> bool;

    /// Get the insert cursor beat position, if any.
    fn insert_cursor_beat(&self) -> Option<f32>;

    /// Get the insert cursor layer index, if any.
    fn insert_cursor_layer_index(&self) -> Option<usize>;

    /// Clear all selection (clips, layers, region, insert cursor).
    fn clear_selection(&mut self);

    /// Zoom to fit all clips in the viewport.
    /// Port of Unity InputHandler.ZoomToFit (lines 906-957).
    fn zoom_to_fit(&mut self);
}
