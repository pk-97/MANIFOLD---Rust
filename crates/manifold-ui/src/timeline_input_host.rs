//! Callback interface for UI-side effects that InputHandler cannot own.
//! Mechanical translation of Assets/Scripts/UI/Timeline/ITimelineInputHost.cs.
//!
//! Implemented by the app layer (WorkspaceController equivalent) as thin delegations.
//! InputHandler calls through this trait for operations that need engine/UI access.

use manifold_foundation::{Beats, ClipId, Seconds};

/// Callback interface for InputHandler → host communication.
/// Port of ITimelineInputHost.cs — every method maps 1:1.
pub trait TimelineInputHost {
    // NOTE: Clip ID parameters use ClipId (typed wrapper) for compile-time safety.
    /// Handle inspector-specific keyboard input (e.g., arrow key stepping for loop duration).
    /// Returns true if the key was consumed by the inspector.
    fn handle_inspector_keyboard(&mut self) -> bool;

    /// Toggle the performance HUD visibility.
    fn toggle_performance_hud(&mut self);

    /// Whether the monitor output window is currently active.
    fn is_monitor_output_active(&self) -> bool;

    /// Close the monitor output window.
    fn close_output_window(&mut self);

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

    /// Select all effects in the active inspector tab. Returns true if handled.
    fn handle_effect_select_all(&mut self) -> bool;

    /// Clear effect selection in the inspector.
    fn clear_effect_selection(&mut self);

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
    fn play_pause(&mut self, insert_cursor_beat: Option<Beats>);

    /// Seek to a specific time in seconds.
    fn seek_to(&mut self, time: Seconds);

    /// Get the current playback beat.
    fn current_beat(&self) -> f32;

    /// Whether playback is active.
    fn is_playing(&self) -> bool;

    /// Select all clips across all layers.
    fn select_all_clips(&mut self);

    /// Copy selected clips to clipboard.
    fn copy_clips(&mut self, clip_ids: &[ClipId]);

    /// Cut selected clips (copy + delete).
    fn cut_clips(&mut self, clip_ids: &[ClipId], has_region: bool);

    /// Paste clips at target position.
    fn paste_clips(&mut self, target_beat: f32, target_layer: i32);

    // ── Finder-paste arbitration (docs/TIMELINE_INGEST_DESIGN.md §2 D4) ──
    // These four stay platform-neutral on purpose: `macos_pasteboard.rs` is
    // the only module allowed to name NSPasteboard, so this trait exposes
    // its results (file URLs, a changeCount, a snapshot) rather than the
    // AppKit types themselves. The arbitration decision itself is a pure
    // function in `input_handler.rs`, unit-tested without any pasteboard.

    /// File URLs currently on the general pasteboard (empty if none — text,
    /// an image, or nothing copied).
    fn pasteboard_file_urls(&self) -> Vec<std::path::PathBuf>;

    /// AppKit's `NSPasteboard.generalPasteboard.changeCount` right now.
    fn pasteboard_change_count(&self) -> i64;

    /// The pasteboard changeCount snapshotted the last time this app copied
    /// clips internally. `None` if no internal copy has happened yet.
    fn internal_clipboard_snapshot(&self) -> Option<i64>;

    /// D5: ingest external files (e.g. a Finder Cmd+C) at `target_beat`,
    /// joining the active layer if it is audio. Routes through the same
    /// `process_dropped_files` path a Finder drag-drop uses.
    fn paste_pasteboard_files(&mut self, file_paths: &[std::path::PathBuf], target_beat: f32);

    /// Duplicate selected clips.
    fn duplicate_clips(&mut self, clip_ids: &[ClipId]);

    /// Delete selected clips (region-aware).
    fn delete_clips(&mut self, clip_ids: &[ClipId], has_region: bool);

    /// Delete a layer by index.
    fn delete_layer(&mut self, layer_index: usize);

    /// Split selected clips at the current playhead beat.
    fn split_clips_at_playhead(&mut self, clip_ids: &[ClipId]);

    /// Extend selected clips by grid step.
    fn extend_clips(&mut self, clip_ids: &[ClipId], grid_step: f32);

    /// Shrink selected clips by grid step.
    fn shrink_clips(&mut self, clip_ids: &[ClipId], grid_step: f32);

    /// Nudge selected clips by beat delta.
    fn nudge_clips(&mut self, clip_ids: &[ClipId], beat_delta: f32);

    /// Move a clip selection across layers by a fixed layer-index delta
    /// (keyboard Up/Down, B14). All-or-nothing across the selection — see
    /// `EditingService::move_clips_across_layers`. One undo entry per press.
    fn move_selection_across_layers(&mut self, clip_ids: &[ClipId], layer_delta: i32);

    /// Toggle mute on selected clips.
    fn toggle_mute_clips(&mut self, clip_ids: &[ClipId]);

    /// Group selected layers.
    fn group_selected_layers(&mut self);

    /// Ungroup the selected group layer.
    fn ungroup_selected_layers(&mut self);

    /// Delete selected layers.
    fn delete_selected_layers(&mut self);

    /// Duplicate selected layers (Ableton-style: deep copy inserted below last selected).
    fn duplicate_selected_layers(&mut self);

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

    /// Start offline video export with current project settings.
    fn start_export(&mut self);

    /// Route an Escape through the overlay driver, dismissing the top-most open
    /// dismissable overlay (modal or dropdown). Returns true if one handled it.
    /// The perf HUD is modeless and does not, so Escape then falls through to
    /// selection clearing.
    fn dismiss_top_overlay(&mut self) -> bool;

    /// Get the current grid step in beats (from viewport zoom level).
    fn grid_step(&self) -> f32;

    /// Navigate the insert cursor (arrow keys). Direction: 0=left, 1=right, 2=up, 3=down.
    /// `is_fine`: true when Shift is held (1/16 beat step).
    /// Returns true if the navigation resulted in a clip auto-select.
    fn navigate_cursor(&mut self, direction: u8, is_fine: bool, grid_step: f32);

    // ── UIState delegation (InputHandler reads selection through host) ──

    /// Get IDs of all selected clips.
    fn get_selected_clip_ids(&self) -> Vec<ClipId>;

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

    /// Zoom to frame the current selection (`Clips` or `TimeRange`) with
    /// margin (B14 `Z`). No-op if there is no selection. Captures the
    /// pre-zoom view so `zoom_back` can restore it.
    fn zoom_to_selection(&mut self);

    /// Restore the view captured by the last `zoom_to_selection` (B14
    /// `Shift+Z`). No-op if no snapshot is stored.
    fn zoom_back(&mut self);

    // ── Timeline markers ─────────────────────────────────────────

    /// Add a marker at the current playhead beat (snapped to grid).
    fn add_marker_at_playhead(&mut self);

    /// Delete all currently selected markers.
    fn delete_selected_markers(&mut self);

    /// Whether any markers are currently selected.
    fn has_selected_markers(&self) -> bool;

    // ── Automation lane editing (P4, `docs/AUTOMATION_LANES_DESIGN.md` §7) ──

    /// Whether a single automation breakpoint is currently selected (set by
    /// a plain click on a dot — `UIState::selected_automation_point`).
    fn has_selected_automation_point(&self) -> bool;

    /// Delete the currently selected automation breakpoint. No-op if none
    /// selected or it no longer resolves (e.g. its lane was cleared).
    fn delete_selected_automation_point(&mut self);

    // ── Automation lane editing — marquee + draw mode (P4 Unit B) ─────

    /// Whether a marquee multi-selection of automation breakpoints is active
    /// (`UIState::selected_automation_points` non-empty). Checked BEFORE
    /// `has_selected_automation_point` in Delete-key priority — a marquee
    /// selection takes precedence over a single click-selection.
    fn has_selected_automation_points(&self) -> bool;

    /// Delete the entire marquee-selected set as ONE undo entry — grouped by
    /// lane, each lane's indices resolved highest-to-lowest so an earlier
    /// removal within that lane never shifts a later target index.
    fn delete_selected_automation_points(&mut self);

    /// Toggle pencil/draw mode (Live's `B`) — while on, dragging inside an
    /// automation lane strip draws points instead of grabbing a dot/segment.
    fn toggle_automation_draw_mode(&mut self);

    /// Whether automation mode is currently showing lane strips — gates the
    /// `B` keybinding so it's a no-op with lanes hidden.
    fn automation_mode_visible(&self) -> bool;

    /// Toggle automation-lane visibility across the timeline (Live's `A`) —
    /// same effect as clicking the transport bar's LANES button
    /// (`PanelAction::ToggleAutomationMode`). Unlike `toggle_automation_draw_mode`,
    /// not gated on current visibility — `A` must work from either state.
    fn toggle_automation_mode_visible(&mut self);
}
