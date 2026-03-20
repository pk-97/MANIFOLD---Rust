//! Implementation of TimelineInputHost for the Application.
//!
//! Wraps Application fields to implement the TimelineInputHost trait.
//! Same split-borrow pattern as AppEditingHost — borrows individual fields
//! so InputHandler, UIState, and viewport can be borrowed separately.

use manifold_editing::commands::clip::MuteClipCommand;
use manifold_editing::service::EditingService;
use manifold_ui::timeline_input_host::TimelineInputHost;
use manifold_ui::ui_state::UIState;
use manifold_ui::cursor_nav;

use crate::ui_root::UIRoot;

/// Wrapper implementing TimelineInputHost by borrowing Application fields.
///
/// Selection (UIState) is available for host methods that need to read/write
/// selection state (paste, duplicate, navigate_cursor, select_all, etc.).
pub struct AppInputHost<'a> {
    pub project: &'a mut manifold_core::project::Project,
    pub content_tx: &'a crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    pub content_state: &'a crate::content_state::ContentState,
    pub ui_root: &'a mut UIRoot,
    pub selection: &'a mut UIState,
    pub active_layer: &'a mut Option<usize>,
    pub needs_rebuild: &'a mut bool,
    pub needs_structural_sync: &'a mut bool,
    pub needs_scroll_rebuild: &'a mut bool,
    pub current_project_path: &'a Option<std::path::PathBuf>,
    pub has_output_window: bool,
    pub pending_close_output: &'a mut bool,
}

impl TimelineInputHost for AppInputHost<'_> {
    fn handle_inspector_keyboard(&mut self) -> bool {
        // Future: inspector arrow key stepping for loop duration.
        // Stub returns false — correct until clip inspector is ported.
        false
    }

    fn toggle_performance_hud(&mut self) {
        self.ui_root.perf_hud.toggle();
        *self.needs_rebuild = true;
    }

    fn is_monitor_output_active(&self) -> bool {
        self.has_output_window
    }

    fn close_output_window(&mut self) {
        *self.pending_close_output = true;
    }

    fn request_rebuild(&mut self) {
        *self.needs_rebuild = true;
    }

    fn on_undo_redo(&mut self) {
        // Unity WorkspaceController lines 378-386:
        //   needsRebuild = true; RefreshAllInspectors();
        //   playbackController.RefreshActiveClips(); playbackController.MarkCompositorDirty();
        //   ApplyProjectResolutionFromFooter(); ApplyProjectFpsFromFooter();
        let _ = self.content_tx.try_send(crate::content_command::ContentCommand::MarkCompositorDirty);

        // TODO: Re-apply resolution/FPS from project settings after undo/redo.
        // Unity calls ApplyProjectResolutionFromFooter() and ApplyProjectFpsFromFooter()
        // to sync render pipeline with potentially changed settings.
        // Requires PlaybackEngine.set_resolution()/set_fps() (not yet ported).

        *self.needs_structural_sync = true;
        *self.needs_rebuild = true;
    }

    fn on_selection_cleared(&mut self) {
        // Unity WorkspaceController.OnSelectionCleared (lines 388-393):
        //   InvalidateAllLayerBitmaps();
        //   ResetAllInspectors();
        //   masterInspector?.Show();
        *self.needs_rebuild = true;
        *self.needs_structural_sync = true;
        *self.needs_scroll_rebuild = true;
    }

    fn mark_compositor_dirty(&mut self) {
        let _ = self.content_tx.try_send(crate::content_command::ContentCommand::MarkCompositorDirty);
    }

    fn invalidate_all_layer_bitmaps(&mut self) {
        *self.needs_scroll_rebuild = true;
    }

    fn update_zoom_label(&mut self) {
        // Zoom label is updated during push_state
    }

    fn get_playhead_viewport_x(&self) -> f32 {
        let beat = self.content_state.current_beat;
        let ppb = self.ui_root.viewport.pixels_per_beat();
        let scroll = self.ui_root.viewport.scroll_x_beats();
        (beat - scroll) * ppb
    }

    fn get_viewport_width(&self) -> f32 {
        self.ui_root.viewport.tracks_rect().width
    }

    fn get_seconds_per_beat(&self) -> f32 {
        let bpm = Some(&*self.project)
            .map(|p| p.settings.bpm)
            .unwrap_or(120.0);
        if bpm > 0.0 { 60.0 / bpm } else { 0.5 }
    }

    fn on_clip_selected(&mut self, _clip_id: &str) {
        *self.needs_structural_sync = true;
    }

    // ── Effect shortcuts: stubs until effect system is ported ────
    // These return false, so InputHandler falls through to clip operations.
    // Correct infrastructure — just needs implementations when effects land.
    fn handle_effect_copy(&mut self) -> bool { false }
    fn handle_effect_cut(&mut self) -> bool { false }
    fn handle_effect_paste(&mut self) -> bool { false }
    fn handle_effect_delete(&mut self) -> bool { false }
    fn handle_effect_group(&mut self) -> bool { false }
    fn handle_effect_ungroup(&mut self) -> bool { false }
    fn clear_effect_selection(&mut self) {}
    fn set_inspector_focus(&mut self, _focused: bool) {}
    fn show_toast(&mut self, message: &str) {
        log::info!("[Toast] {}", message);
    }

    fn undo(&mut self) {
        crate::ui_bridge::undo(self.content_tx);
    }

    fn redo(&mut self) {
        crate::ui_bridge::redo(self.content_tx);
    }

    fn save_project(&mut self) {
        // Save requires the rfd dialog and window handle, which are owned by
        // Application (not borrowed here). For now, the actual save logic
        // stays in the legacy block in app.rs. InputHandler returns false
        // for Cmd+S so it falls through to the legacy handler.
        //
        // TODO: When the legacy block is deleted, refactor save to use a
        // flag that Application picks up after the host call returns.
        log::info!("Save requested via keyboard shortcut");
    }

    fn open_project(&mut self) {
        // Same as save — needs rfd dialog + window handle.
        log::info!("Open requested via keyboard shortcut");
    }

    fn new_project(&mut self) {
        // Same as save — needs to create project + initialize engine.
        log::info!("New project requested via keyboard shortcut");
    }

    fn play_pause(&mut self, insert_cursor_beat: Option<f32>) {
        if self.content_state.is_playing {
            let _ = self.content_tx.try_send(crate::content_command::ContentCommand::Pause);
        } else {
            // Unity: if paused and insert cursor exists, seek to cursor first (Ableton behavior)
            if let Some(beat) = insert_cursor_beat {
                let time = beat * (60.0 / self.project.settings.bpm);
                let _ = self.content_tx.try_send(crate::content_command::ContentCommand::SeekTo(time));
            }
            let _ = self.content_tx.try_send(crate::content_command::ContentCommand::Play);
        }
    }

    fn seek_to(&mut self, time: f32) {
        if time == f32::MAX {
            // Sentinel for "seek to end" — Unity InputHandler line 380-390
            // Uses beat_to_timeline_time for tempo map consistency (Step 8 fix)
            if let Some(project) = Some(&*self.project) {
                let mut max_beat: f32 = 0.0;
                for layer in &project.timeline.layers {
                    for clip in &layer.clips {
                        let end = clip.start_beat + clip.duration_beats;
                        if end > max_beat { max_beat = end; }
                    }
                }
                let end_time = max_beat * (60.0 / self.project.settings.bpm);
                let _ = self.content_tx.try_send(crate::content_command::ContentCommand::SeekTo(end_time));
            }
        } else {
            let _ = self.content_tx.try_send(crate::content_command::ContentCommand::SeekTo(time));
        }
    }

    fn current_beat(&self) -> f32 {
        self.content_state.current_beat
    }

    fn is_playing(&self) -> bool {
        self.content_state.is_playing
    }

    fn select_all_clips(&mut self) {
        // Unity EditingService.SelectAllClips (lines 264-276)
        if let Some(project) = Some(&*self.project) {
            self.selection.clear_selection();
            for layer in &project.timeline.layers {
                for clip in &layer.clips {
                    self.selection.selected_clip_ids.insert(clip.id.clone());
                }
            }
            self.selection.primary_selected_clip_id =
                self.selection.selected_clip_ids.iter().next().cloned();
            self.selection.selection_version += 1;

            // Unity line 275: compute bounding region from all selected clips
            crate::ui_bridge::update_region_from_clip_selection_inline(
                self.selection, project);
        }
        *self.needs_structural_sync = true;
    }

    fn copy_clips(&mut self, clip_ids: &[String]) {
        // Region-aware copy: trim clips at region boundaries (Unity CopySelectedClips)
        let region = if self.selection.has_region() {
            Some(self.selection.get_region().clone())
        } else {
            None
        };
        if let Some(project) = Some(&*self.project) {
            let spb = 60.0 / project.settings.bpm.max(1.0);
            // TODO: clipboard operations need EditingService refactor
            log::info!("Copy clips requested");
        }
    }

    fn cut_clips(&mut self, clip_ids: &[String], has_region: bool) {
        // Unity: copy first (region-aware), then delete
        let region = if has_region {
            Some(self.selection.get_region().clone())
        } else {
            None
        };
        if let Some(project) = Some(&*self.project) {
            let spb = 60.0 / project.settings.bpm.max(1.0);
            // TODO: clipboard operations need EditingService refactor
            log::info!("Copy clips requested");
        }
        if let Some(project) = Some(&mut *self.project) {
            let spb = 60.0 / project.settings.bpm;
            // Step 4i: pass actual region from UIState when active
            let region = if has_region {
                Some(self.selection.get_region().clone())
            } else {
                None
            };
            let commands = EditingService::delete_clips(project, clip_ids, region.as_ref(), spb);
            { for mut c in commands { c.execute(project); let _ = self.content_tx.try_send(crate::content_command::ContentCommand::Record(c)); } }
        }
        self.selection.clear_selection();
    }

    fn paste_clips(&mut self, target_beat: f32, target_layer: i32) {
        // Unity EditingService.PasteClips (line 660-667):
        // After paste, select all pasted clips and update region.
        if let Some(project) = Some(&mut *self.project) {
            let spb = 60.0 / project.settings.bpm;
            let result = // TODO: paste operations need EditingService refactor
            manifold_editing::service::PasteResult { commands: Vec::new(), pasted_clip_ids: Vec::new(), skip_reason: None, skipped_count: 0 };
            if !result.commands.is_empty() {
                // Paste not yet wired to content thread
                // Step 4g: select pasted clips and update region
                self.selection.clear_selection();
                for id in result.pasted_clip_ids {
                    self.selection.selected_clip_ids.insert(id);
                }
                self.selection.primary_selected_clip_id =
                    self.selection.selected_clip_ids.iter().next().cloned();
                self.selection.selection_version += 1;
                crate::ui_bridge::update_region_from_clip_selection_inline(
                    self.selection, project);
            }
        }
        *self.needs_structural_sync = true;
    }

    fn duplicate_clips(&mut self, clip_ids: &[String]) {
        // Unity EditingService.DuplicateSelectedClips (line 767-778):
        // After duplicate, select the new clips and update region.
        if let Some(project) = Some(&mut *self.project) {
            let mut region = manifold_core::selection::SelectionRegion::default();
            let mut min_beat = f32::MAX;
            let mut max_beat = f32::MIN;
            for layer in &project.timeline.layers {
                for clip in &layer.clips {
                    if clip_ids.contains(&clip.id) {
                        min_beat = min_beat.min(clip.start_beat);
                        max_beat = max_beat.max(clip.start_beat + clip.duration_beats);
                    }
                }
            }
            if max_beat > min_beat {
                region.is_active = true;
                region.start_beat = min_beat;
                region.end_beat = max_beat;
            }
            // Snapshot existing IDs to find new ones after execute
            let before_ids: std::collections::HashSet<String> = project.timeline.layers.iter()
                .flat_map(|l| l.clips.iter().map(|c| c.id.clone()))
                .collect();

            let spb = 60.0 / project.settings.bpm.max(1.0);
            let commands = EditingService::duplicate_clips(project, clip_ids, &region, spb);
            if !commands.is_empty() {
                { for mut c in commands { c.execute(project); let _ = self.content_tx.try_send(crate::content_command::ContentCommand::Record(c)); } }

                // Step 4h: find newly created clips and select them
                let new_ids: Vec<String> = project.timeline.layers.iter()
                    .flat_map(|l| l.clips.iter()
                        .filter(|c| !before_ids.contains(&c.id))
                        .map(|c| c.id.clone()))
                    .collect();

                self.selection.clear_selection();
                for id in &new_ids {
                    self.selection.selected_clip_ids.insert(id.clone());
                }
                self.selection.primary_selected_clip_id = new_ids.first().cloned();
                self.selection.selection_version += 1;
                crate::ui_bridge::update_region_from_clip_selection_inline(
                    self.selection, project);
            }
        }
        *self.needs_structural_sync = true;
    }

    fn delete_clips(&mut self, clip_ids: &[String], has_region: bool) {
        if let Some(project) = Some(&mut *self.project) {
            let spb = 60.0 / project.settings.bpm;
            // Step 4i: pass actual region from UIState when active
            let region = if has_region {
                Some(self.selection.get_region().clone())
            } else {
                None
            };
            let commands = EditingService::delete_clips(project, clip_ids, region.as_ref(), spb);
            { for mut c in commands { c.execute(project); let _ = self.content_tx.try_send(crate::content_command::ContentCommand::Record(c)); } }
        }
        *self.needs_structural_sync = true;
    }

    fn delete_layer(&mut self, layer_index: usize) {
        if let Some(project) = Some(&mut *self.project) {
            if project.timeline.layers.len() > 1 {
                if let Some(layer) = project.timeline.layers.get(layer_index) {
                    let layer_clone = layer.clone();
                    let cmd = manifold_editing::commands::layer::DeleteLayerCommand::new(layer_clone, layer_index);
                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = self.content_tx.try_send(crate::content_command::ContentCommand::Record(boxed)); }
                }
            }
        }
        *self.needs_rebuild = true;
    }

    fn split_clips_at_playhead(&mut self, clip_ids: &[String]) {
        let beat = self.content_state.current_beat;
        if let Some(project) = Some(&mut *self.project) {
            let spb = 60.0 / project.settings.bpm;
            let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
            for id in clip_ids {
                if let Some(cmd) = EditingService::split_clip_at_beat(project, id, beat, spb) {
                    commands.push(cmd);
                }
            }
            if !commands.is_empty() {
                { for mut c in commands { c.execute(project); let _ = self.content_tx.try_send(crate::content_command::ContentCommand::Record(c)); } }
            }
        }
    }

    fn extend_clips(&mut self, clip_ids: &[String], grid_step: f32) {
        if let Some(project) = Some(&mut *self.project) {
            let commands = EditingService::extend_clips_by_grid(project, clip_ids, grid_step);
            if !commands.is_empty() {
                { for mut c in commands { c.execute(project); let _ = self.content_tx.try_send(crate::content_command::ContentCommand::Record(c)); } }
            }
        }
    }

    fn shrink_clips(&mut self, clip_ids: &[String], grid_step: f32) {
        if let Some(project) = Some(&mut *self.project) {
            let commands = EditingService::shrink_clips_by_grid(project, clip_ids, grid_step);
            if !commands.is_empty() {
                { for mut c in commands { c.execute(project); let _ = self.content_tx.try_send(crate::content_command::ContentCommand::Record(c)); } }
            }
        }
    }

    fn nudge_clips(&mut self, clip_ids: &[String], beat_delta: f32) {
        if let Some(project) = Some(&mut *self.project) {
            let spb = 60.0 / project.settings.bpm;
            let commands = EditingService::nudge_clips(project, clip_ids, beat_delta, spb);
            if !commands.is_empty() {
                { for mut c in commands { c.execute(project); let _ = self.content_tx.try_send(crate::content_command::ContentCommand::Record(c)); } }
            }
        }
        *self.needs_structural_sync = true;
    }

    fn toggle_mute_clips(&mut self, clip_ids: &[String]) {
        // Unity EditingService.ToggleMuteSelectedClips (line 418-448):
        // Group-mute semantics: if ANY unmuted → mute ALL, else unmute ALL.
        // Records undo via MuteClipCommand. Marks compositor dirty.
        if let Some(project) = Some(&mut *self.project) {
            // First pass: collect current mute state for each clip
            let mut clip_states: Vec<(String, bool)> = Vec::new();
            for layer in &project.timeline.layers {
                for clip in &layer.clips {
                    if clip_ids.contains(&clip.id) {
                        clip_states.push((clip.id.clone(), clip.is_muted));
                    }
                }
            }

            // Determine target: if ANY unmuted → mute all, else unmute all
            let any_unmuted = clip_states.iter().any(|(_, muted)| !muted);
            let new_muted = any_unmuted;

            // Build commands for clips that need to change
            let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
            for (id, old_muted) in &clip_states {
                if *old_muted != new_muted {
                    commands.push(Box::new(MuteClipCommand::new(
                        id.clone(), *old_muted, new_muted,
                    )));
                }
            }

            if !commands.is_empty() {
                let label = if new_muted { "Mute clips" } else { "Unmute clips" };
                { for mut c in commands { c.execute(project); let _ = self.content_tx.try_send(crate::content_command::ContentCommand::Record(c)); } }
            }
        }
        let _ = self.content_tx.try_send(crate::content_command::ContentCommand::MarkCompositorDirty);
        *self.needs_structural_sync = true;
        *self.needs_rebuild = true;
    }

    fn group_selected_layers(&mut self) {
        // Port of Unity EditingService.GroupSelectedLayers.
        // Requires >= 2 selected, none nested or already groups.
        if self.selection.layer_selection_count() < 2 {
            return;
        }

        let selected_ids: Vec<String> = self.selection.selected_layer_ids.iter().cloned().collect();

        if let Some(project) = Some(&*self.project) {
            // Validate: none are nested (have parent) or group layers
            let mut layers_to_group = Vec::new();
            for layer in &project.timeline.layers {
                if selected_ids.contains(&layer.layer_id) {
                    if layer.parent_layer_id.is_some() || layer.is_group() {
                        return; // Validation failure
                    }
                    layers_to_group.push(layer.layer_id.clone());
                }
            }
            if layers_to_group.len() < 2 {
                return;
            }

            // Snapshot current order for undo
            let original_order = project.timeline.layers.clone();
            let cmd = manifold_editing::commands::layer::GroupLayersCommand::new(
                layers_to_group, original_order,
            );

            if let Some(project) = Some(&mut *self.project) {
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = self.content_tx.try_send(crate::content_command::ContentCommand::Record(boxed)); }
            }
        }

        self.selection.clear_selection();
        let _ = self.content_tx.try_send(crate::content_command::ContentCommand::MarkCompositorDirty);
        *self.needs_rebuild = true;
        *self.needs_structural_sync = true;
    }

    fn delete_selected_layers(&mut self) {
        // Port of Unity EditingService.DeleteSelectedLayers.
        // Deletes selected layers in reverse index order, preserves at least 1 layer.
        if self.selection.layer_selection_count() == 0 {
            return;
        }

        let selected_ids: Vec<String> = self.selection.selected_layer_ids.iter().cloned().collect();

        if let Some(project) = Some(&mut *self.project) {
            // Find indices to delete (in reverse order for safe removal)
            let mut indices: Vec<usize> = Vec::new();
            for (i, layer) in project.timeline.layers.iter().enumerate() {
                if selected_ids.contains(&layer.layer_id) {
                    indices.push(i);
                }
            }

            // Don't delete all layers — keep at least one
            if indices.len() >= project.timeline.layers.len() {
                indices.pop();
            }

            if indices.is_empty() {
                return;
            }

            // Delete in reverse order (highest index first) for correct indexing
            indices.sort_unstable();
            indices.reverse();

            let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
            for &idx in &indices {
                if idx < project.timeline.layers.len() {
                    let layer_clone = project.timeline.layers[idx].clone();
                    let cmd = manifold_editing::commands::layer::DeleteLayerCommand::new(
                        layer_clone, idx,
                    );
                    commands.push(Box::new(cmd));
                }
            }

            if !commands.is_empty() {
                { for mut c in commands { c.execute(project); let _ = self.content_tx.try_send(crate::content_command::ContentCommand::Record(c)); } }
            }
        }

        self.selection.clear_selection();
        let _ = self.content_tx.try_send(crate::content_command::ContentCommand::MarkCompositorDirty);
        *self.needs_rebuild = true;
        *self.needs_structural_sync = true;
    }

    fn layer_count(&self) -> usize {
        Some(&*self.project)
            .map(|p| p.timeline.layers.len())
            .unwrap_or(0)
    }

    fn project_beats_per_bar(&self) -> u32 {
        Some(&*self.project)
            .map(|p| p.settings.time_signature_numerator.max(1) as u32)
            .unwrap_or(4)
    }

    fn set_export_in_at_playhead(&mut self) {
        // Unity InputHandler.SetExportInAtPlayhead (lines 615-628):
        // Snap to grid before applying.
        let beat = self.content_state.current_beat;
        if let Some(project) = Some(&mut *self.project) {
            let bpb = project.settings.time_signature_numerator.max(1) as u32;
            let snapped = self.ui_root.viewport.mapper().snap_beat_to_grid(beat, bpb);
            project.timeline.export_in_beat = snapped;
            project.timeline.export_range_enabled = true;
        }
    }

    fn set_export_out_at_playhead(&mut self) {
        // Unity InputHandler.SetExportOutAtPlayhead (lines 630-643):
        // Snap to grid before applying.
        let beat = self.content_state.current_beat;
        if let Some(project) = Some(&mut *self.project) {
            let bpb = project.settings.time_signature_numerator.max(1) as u32;
            let snapped = self.ui_root.viewport.mapper().snap_beat_to_grid(beat, bpb);
            project.timeline.export_out_beat = snapped;
            project.timeline.export_range_enabled = true;
        }
    }

    fn clear_export_in(&mut self) {
        // Unity InputHandler.ClearExportIn (lines 645-662):
        // If no out-point → clear entire range.
        // If out-point exists → reset in to 0, keep range enabled.
        if let Some(project) = Some(&mut *self.project) {
            if project.timeline.export_out_beat <= 0.0 {
                // No out-point — clear entire range
                project.timeline.export_in_beat = 0.0;
                project.timeline.export_out_beat = 0.0;
                project.timeline.export_range_enabled = false;
            } else {
                // Out-point exists — reset in to 0 but keep range
                project.timeline.export_in_beat = 0.0;
            }
        }
    }

    fn clear_export_out(&mut self) {
        // Unity InputHandler.ClearExportOut (lines 664-677):
        // If no range active → no-op.
        // If range active → clear entire range.
        if let Some(project) = Some(&mut *self.project) {
            if !project.timeline.export_range_enabled {
                return; // no-op
            }
            project.timeline.export_in_beat = 0.0;
            project.timeline.export_out_beat = 0.0;
            project.timeline.export_range_enabled = false;
        }
    }

    fn dismiss_context_menu(&mut self) {
        self.ui_root.dropdown.close(&mut self.ui_root.tree);
    }

    fn has_context_menu(&self) -> bool {
        self.ui_root.dropdown.is_open()
    }

    fn grid_step(&self) -> f32 {
        self.ui_root.viewport.grid_step()
    }

    fn navigate_cursor(&mut self, direction: u8, is_fine: bool, grid_step: f32) {
        // Unity InputHandler.NavigateInsertCursor (lines 523-595)
        let dir = match direction {
            0 => cursor_nav::Direction::Left,
            1 => cursor_nav::Direction::Right,
            2 => cursor_nav::Direction::Up,
            3 => cursor_nav::Direction::Down,
            _ => return,
        };

        let mapper = self.ui_root.viewport.mapper();
        let layer_count = mapper.layer_count();
        let mut layers = Vec::with_capacity(layer_count);
        let mut clips = Vec::new();

        if let Some(project) = Some(&*self.project) {
            for (i, layer) in project.timeline.layers.iter().enumerate() {
                layers.push(cursor_nav::NavLayerInfo {
                    index: i,
                    height: mapper.get_layer_height(i),
                });
                for clip in &layer.clips {
                    clips.push(cursor_nav::NavClipInfo {
                        clip_id: clip.id.clone(),
                        layer_index: i,
                        start_beat: clip.start_beat,
                        end_beat: clip.start_beat + clip.duration_beats,
                    });
                }
            }
        }

        // Step 4f: read cursor position from UIState (not viewport scroll)
        let current_beat = self.selection.insert_cursor_beat
            .unwrap_or(self.content_state.current_beat);
        let current_layer = self.selection.insert_cursor_layer_index
            .or(*self.active_layer)
            .unwrap_or(0);

        let result = cursor_nav::navigate_cursor(
            dir, current_beat, current_layer, grid_step, is_fine, &layers, &clips,
        );
        match result {
            cursor_nav::NavResult::SetCursor { beat, layer } => {
                self.selection.set_insert_cursor(beat, layer);
                *self.active_layer = Some(layer);
            }
            cursor_nav::NavResult::SelectClip(clip_id) => {
                // Find the clip's layer for proper selection
                let li = Some(&*self.project)
                    .and_then(|p| p.timeline.layers.iter().enumerate()
                        .find_map(|(i, l)| l.clips.iter()
                            .any(|c| c.id == clip_id).then_some(i)))
                    .unwrap_or(0);
                self.selection.select_clip(clip_id, li);
                *self.active_layer = Some(li);
            }
            cursor_nav::NavResult::NoChange => {}
        }

        *self.needs_rebuild = true;
        *self.needs_scroll_rebuild = true;
    }

    // ── UIState delegation ──────────────────────────────────────

    fn get_selected_clip_ids(&self) -> Vec<String> {
        self.selection.get_selected_clip_ids()
    }

    fn selection_count(&self) -> usize {
        self.selection.selection_count()
    }

    fn layer_selection_count(&self) -> usize {
        self.selection.layer_selection_count()
    }

    fn has_region(&self) -> bool {
        self.selection.has_region()
    }

    fn insert_cursor_beat(&self) -> Option<f32> {
        self.selection.insert_cursor_beat
    }

    fn insert_cursor_layer_index(&self) -> Option<usize> {
        self.selection.insert_cursor_layer_index
    }

    fn clear_selection(&mut self) {
        self.selection.clear_selection();
    }

    fn zoom_to_fit(&mut self) {
        // Unity InputHandler.ZoomToFit (lines 906-957):
        // Arbitrary ppb, center scroll, no-clips fallback.
        let viewport_width = self.ui_root.viewport.tracks_rect().width;
        if viewport_width <= 0.0 { return; }

        let project = match Some(&*self.project) {
            Some(p) => p,
            None => return,
        };

        let mut min_beat = f32::MAX;
        let mut max_beat = f32::MIN;
        let mut clip_count = 0;
        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if clip.start_beat < min_beat { min_beat = clip.start_beat; }
                let end = clip.start_beat + clip.duration_beats;
                if end > max_beat { max_beat = end; }
                clip_count += 1;
            }
        }

        if clip_count == 0 {
            // No clips — reset to default zoom, scroll to start
            let levels = &manifold_ui::color::ZOOM_LEVELS;
            let default_idx = levels.len() / 2; // middle of zoom range
            self.ui_root.viewport.set_zoom(levels[default_idx]);
            self.ui_root.viewport.set_scroll(0.0, 0.0);
            *self.needs_scroll_rebuild = true;
            return;
        }

        let extent_beats = max_beat - min_beat;
        // 10% padding on each side (min 1 beat)
        let padding = (extent_beats * 0.1).max(1.0);
        let fit_beats = extent_beats + padding * 2.0;

        // Calculate ideal ppb — arbitrary float, NOT nearest preset
        let max_ppb = *manifold_ui::color::ZOOM_LEVELS.last().unwrap_or(&200.0);
        let ideal_ppb = (viewport_width / fit_beats).clamp(1.0, max_ppb);

        self.ui_root.viewport.set_zoom(ideal_ppb);

        // Center-scroll on clip extent
        let center_beat = min_beat + extent_beats * 0.5;
        let center_pixel = center_beat * ideal_ppb;
        let scroll_beat = ((center_pixel - viewport_width * 0.5) / ideal_ppb).max(0.0);
        self.ui_root.viewport.set_scroll(scroll_beat, 0.0);

        *self.needs_scroll_rebuild = true;
    }
}
