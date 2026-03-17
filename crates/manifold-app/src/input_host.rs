//! Implementation of TimelineInputHost for the Application.
//!
//! Wraps Application fields to implement the TimelineInputHost trait.
//! Same split-borrow pattern as AppEditingHost — borrows individual fields
//! so InputHandler, UIState, and viewport can be borrowed separately.

use manifold_editing::service::EditingService;
use manifold_playback::engine::PlaybackEngine;
use manifold_ui::timeline_input_host::TimelineInputHost;
use manifold_ui::cursor_nav;

use crate::ui_root::UIRoot;

/// Wrapper implementing TimelineInputHost by borrowing Application fields.
pub struct AppInputHost<'a> {
    pub engine: &'a mut PlaybackEngine,
    pub editing: &'a mut EditingService,
    pub ui_root: &'a mut UIRoot,
    pub active_layer: &'a mut Option<usize>,
    pub needs_rebuild: &'a mut bool,
    pub needs_structural_sync: &'a mut bool,
    pub needs_scroll_rebuild: &'a mut bool,
    pub current_project_path: &'a Option<std::path::PathBuf>,
    // Selection is passed separately to handle_keyboard_input, not through the host
}

impl TimelineInputHost for AppInputHost<'_> {
    fn handle_inspector_keyboard(&mut self) -> bool {
        // Future: inspector arrow key stepping for loop duration
        false
    }

    fn toggle_performance_hud(&mut self) {
        self.ui_root.perf_hud.toggle();
        *self.needs_rebuild = true;
    }

    fn is_monitor_output_active(&self) -> bool {
        // Future: check if output window is active
        false
    }

    fn request_rebuild(&mut self) {
        *self.needs_rebuild = true;
    }

    fn on_undo_redo(&mut self) {
        self.engine.mark_compositor_dirty(0.0); // realtime will be set by caller
        *self.needs_structural_sync = true;
    }

    fn on_selection_cleared(&mut self) {
        *self.needs_structural_sync = true;
    }

    fn mark_compositor_dirty(&mut self) {
        self.engine.mark_compositor_dirty(0.0);
    }

    fn invalidate_all_layer_bitmaps(&mut self) {
        *self.needs_scroll_rebuild = true;
    }

    fn update_zoom_label(&mut self) {
        // Zoom label is updated during push_state
    }

    fn get_playhead_viewport_x(&self) -> f32 {
        let beat = self.engine.current_beat();
        let ppb = self.ui_root.viewport.pixels_per_beat();
        let scroll = self.ui_root.viewport.scroll_x_beats();
        (beat - scroll) * ppb
    }

    fn get_viewport_width(&self) -> f32 {
        self.ui_root.viewport.tracks_rect().width
    }

    fn get_seconds_per_beat(&self) -> f32 {
        let bpm = self.engine.project()
            .map(|p| p.settings.bpm)
            .unwrap_or(120.0);
        if bpm > 0.0 { 60.0 / bpm } else { 0.5 }
    }

    fn on_clip_selected(&mut self, _clip_id: &str) {
        *self.needs_structural_sync = true;
    }

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
        crate::ui_bridge::undo(self.engine, self.editing);
    }

    fn redo(&mut self) {
        crate::ui_bridge::redo(self.engine, self.editing);
    }

    fn save_project(&mut self) {
        // Save is handled at Application level — set flag for app to pick up
        // For now, log (actual save uses rfd dialog which needs window handle)
        log::info!("Save requested via keyboard shortcut");
    }

    fn open_project(&mut self) {
        log::info!("Open requested via keyboard shortcut");
    }

    fn new_project(&mut self) {
        log::info!("New project requested via keyboard shortcut");
    }

    fn play_pause(&mut self, insert_cursor_beat: Option<f32>) {
        if self.engine.is_playing() {
            self.engine.pause();
        } else {
            if let Some(beat) = insert_cursor_beat {
                let time = self.engine.beat_to_timeline_time(beat);
                self.engine.seek_to(time);
            }
            self.engine.play();
        }
    }

    fn seek_to(&mut self, time: f32) {
        if time == f32::MAX {
            // Sentinel for "seek to end"
            if let Some(project) = self.engine.project() {
                let mut max_beat: f32 = 0.0;
                for layer in &project.timeline.layers {
                    for clip in &layer.clips {
                        let end = clip.start_beat + clip.duration_beats;
                        if end > max_beat { max_beat = end; }
                    }
                }
                let bpm = project.settings.bpm;
                let end_time = max_beat * (60.0 / bpm);
                self.engine.seek_to(end_time);
            }
        } else {
            self.engine.seek_to(time);
        }
    }

    fn current_beat(&self) -> f32 {
        self.engine.current_beat()
    }

    fn is_playing(&self) -> bool {
        self.engine.is_playing()
    }

    fn select_all_clips(&mut self) {
        // Handled directly on UIState by the caller
        // (InputHandler accesses ui_state directly)
    }

    fn copy_clips(&mut self, clip_ids: &[String]) {
        if let Some(project) = self.engine.project() {
            self.editing.copy_clips(project, clip_ids);
        }
    }

    fn cut_clips(&mut self, clip_ids: &[String], has_region: bool) {
        if let Some(project) = self.engine.project() {
            self.editing.copy_clips(project, clip_ids);
        }
        if let Some(project) = self.engine.project_mut() {
            let spb = 60.0 / project.settings.bpm;
            let region = if has_region {
                // TODO: pass region from UIState
                None
            } else {
                None
            };
            let commands = EditingService::delete_clips(project, clip_ids, region.as_ref(), spb);
            self.editing.execute_batch(commands, "Cut clips".into(), project);
        }
    }

    fn paste_clips(&mut self, target_beat: f32, target_layer: i32) {
        if let Some(project) = self.engine.project_mut() {
            let spb = 60.0 / project.settings.bpm;
            let result = self.editing.paste_clips(project, target_beat, target_layer, spb);
            if !result.commands.is_empty() {
                self.editing.execute_batch(result.commands, "Paste clips".into(), project);
            }
        }
        *self.needs_structural_sync = true;
    }

    fn duplicate_clips(&mut self, clip_ids: &[String]) {
        if let Some(project) = self.engine.project_mut() {
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
            let commands = EditingService::duplicate_clips(project, clip_ids, &region);
            if !commands.is_empty() {
                self.editing.execute_batch(commands, "Duplicate clips".into(), project);
            }
        }
        *self.needs_structural_sync = true;
    }

    fn delete_clips(&mut self, clip_ids: &[String], has_region: bool) {
        if let Some(project) = self.engine.project_mut() {
            let spb = 60.0 / project.settings.bpm;
            let region = if has_region {
                None // TODO: pass region from UIState
            } else {
                None
            };
            let commands = EditingService::delete_clips(project, clip_ids, region.as_ref(), spb);
            self.editing.execute_batch(commands, "Delete clips".into(), project);
        }
        *self.needs_structural_sync = true;
    }

    fn delete_layer(&mut self, layer_index: usize) {
        if let Some(project) = self.engine.project_mut() {
            if project.timeline.layers.len() > 1 {
                if let Some(layer) = project.timeline.layers.get(layer_index) {
                    let layer_clone = layer.clone();
                    let cmd = manifold_editing::commands::layer::DeleteLayerCommand::new(layer_clone, layer_index);
                    self.editing.execute(Box::new(cmd), project);
                }
            }
        }
        *self.needs_rebuild = true;
    }

    fn split_clips_at_playhead(&mut self, clip_ids: &[String]) {
        let beat = self.engine.current_beat();
        if let Some(project) = self.engine.project_mut() {
            let spb = 60.0 / project.settings.bpm;
            let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
            for id in clip_ids {
                if let Some(cmd) = EditingService::split_clip_at_beat(project, id, beat, spb) {
                    commands.push(cmd);
                }
            }
            if !commands.is_empty() {
                self.editing.execute_batch(commands, "Split clips".into(), project);
            }
        }
    }

    fn extend_clips(&mut self, clip_ids: &[String], grid_step: f32) {
        if let Some(project) = self.engine.project_mut() {
            let commands = EditingService::extend_clips_by_grid(project, clip_ids, grid_step);
            if !commands.is_empty() {
                self.editing.execute_batch(commands, "Extend clips".into(), project);
            }
        }
    }

    fn shrink_clips(&mut self, clip_ids: &[String], grid_step: f32) {
        if let Some(project) = self.engine.project_mut() {
            let commands = EditingService::shrink_clips_by_grid(project, clip_ids, grid_step);
            if !commands.is_empty() {
                self.editing.execute_batch(commands, "Shrink clips".into(), project);
            }
        }
    }

    fn nudge_clips(&mut self, clip_ids: &[String], beat_delta: f32) {
        if let Some(project) = self.engine.project_mut() {
            let spb = 60.0 / project.settings.bpm;
            let commands = EditingService::nudge_clips(project, clip_ids, beat_delta, spb);
            if !commands.is_empty() {
                self.editing.execute_batch(commands, "Nudge clips".into(), project);
            }
        }
        *self.needs_structural_sync = true;
    }

    fn toggle_mute_clips(&mut self, clip_ids: &[String]) {
        if let Some(project) = self.engine.project_mut() {
            for id in clip_ids {
                if let Some(clip) = project.timeline.find_clip_by_id_mut(id) {
                    clip.is_muted = !clip.is_muted;
                }
            }
        }
        *self.needs_structural_sync = true;
        *self.needs_rebuild = true;
    }

    fn group_selected_layers(&mut self) {
        log::debug!("Group selected layers (stub)");
    }

    fn delete_selected_layers(&mut self) {
        log::debug!("Delete selected layers (stub)");
    }

    fn layer_count(&self) -> usize {
        self.engine.project()
            .map(|p| p.timeline.layers.len())
            .unwrap_or(0)
    }

    fn project_beats_per_bar(&self) -> u32 {
        self.engine.project()
            .map(|p| p.settings.time_signature_numerator.max(1) as u32)
            .unwrap_or(4)
    }

    fn set_export_in_at_playhead(&mut self) {
        let beat = self.engine.current_beat();
        if let Some(project) = self.engine.project_mut() {
            let bpb = project.settings.time_signature_numerator.max(1) as u32;
            let snapped = self.ui_root.viewport.mapper().snap_beat_to_grid(beat, bpb);
            project.timeline.export_in_beat = snapped;
            project.timeline.export_range_enabled = true;
        }
    }

    fn set_export_out_at_playhead(&mut self) {
        let beat = self.engine.current_beat();
        if let Some(project) = self.engine.project_mut() {
            let bpb = project.settings.time_signature_numerator.max(1) as u32;
            let snapped = self.ui_root.viewport.mapper().snap_beat_to_grid(beat, bpb);
            project.timeline.export_out_beat = snapped;
            project.timeline.export_range_enabled = true;
        }
    }

    fn clear_export_in(&mut self) {
        if let Some(project) = self.engine.project_mut() {
            project.timeline.export_in_beat = 0.0;
            project.timeline.export_range_enabled = false;
        }
    }

    fn clear_export_out(&mut self) {
        if let Some(project) = self.engine.project_mut() {
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
        // Delegate to cursor_nav module with layer/clip data from project
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

        if let Some(project) = self.engine.project() {
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

        // Get current cursor position
        let beat = self.ui_root.viewport.scroll_x_beats(); // fallback
        let layer = 0usize; // fallback
        // TODO: read from UIState (passed separately)

        let result = cursor_nav::navigate_cursor(dir, beat, layer, grid_step, is_fine, &layers, &clips);
        match result {
            cursor_nav::NavResult::SetCursor { beat, layer } => {
                // Applied by caller on UIState
                log::debug!("Navigate cursor: beat={}, layer={}", beat, layer);
            }
            cursor_nav::NavResult::SelectClip(clip_id) => {
                log::debug!("Navigate cursor: auto-select clip {}", clip_id);
            }
            cursor_nav::NavResult::NoChange => {}
        }

        *self.needs_scroll_rebuild = true;
    }
}
