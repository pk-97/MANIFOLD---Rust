//! Implementation of TimelineEditingHost for the Application.
//!
//! This bridges the manifold-ui trait (which can't depend on manifold-editing)
//! with the concrete engine, editing service, and command system.
//!
//! The wrapper struct `AppEditingHost` borrows individual Application fields
//! to avoid borrowing the entire Application — this lets the overlay
//! simultaneously borrow ui_root and selection from Application.

use std::collections::HashSet;

use manifold_core::selection::SelectionRegion;
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::service::EditingService;
use manifold_playback::engine::PlaybackEngine;

use manifold_ui::node::Vec2;
use manifold_ui::cursors::{CursorManager, TimelineCursor as UICursor};
use manifold_ui::timeline_editing_host::{
    TimelineEditingHost, TimelineCursor, ClipRef, RegionSplitResult,
};

/// Wrapper that implements TimelineEditingHost by borrowing Application fields.
///
/// Created on the fly when calling InteractionOverlay methods:
/// ```ignore
/// let mut host = AppEditingHost::new(&mut self.engine, &mut self.editing_service, ...);
/// self.ui_root.overlay.on_pointer_click(pos, ..., &mut host, &mut self.selection, &self.ui_root.viewport);
/// ```
pub struct AppEditingHost<'a> {
    pub engine: &'a mut PlaybackEngine,
    pub editing: &'a mut EditingService,
    pub cursor_manager: &'a mut CursorManager,
    pub active_layer: &'a mut Option<usize>,
    pub needs_rebuild: &'a mut bool,
    pub needs_structural_sync: &'a mut bool,
    pub needs_scroll_rebuild: &'a mut bool,

    // Command batch accumulator (for begin_command_batch / commit_command_batch)
    command_batch: Vec<Box<dyn Command>>,
}

impl<'a> AppEditingHost<'a> {
    pub fn new(
        engine: &'a mut PlaybackEngine,
        editing: &'a mut EditingService,
        cursor_manager: &'a mut CursorManager,
        active_layer: &'a mut Option<usize>,
        needs_rebuild: &'a mut bool,
        needs_structural_sync: &'a mut bool,
        needs_scroll_rebuild: &'a mut bool,
    ) -> Self {
        Self {
            engine,
            editing,
            cursor_manager,
            active_layer,
            needs_rebuild,
            needs_structural_sync,
            needs_scroll_rebuild,
            command_batch: Vec::new(),
        }
    }
}

impl TimelineEditingHost for AppEditingHost<'_> {
    // ── Data access ─────────────────────────────────────────────

    fn layer_count(&self) -> usize {
        self.engine.project()
            .map(|p| p.timeline.layers.len())
            .unwrap_or(0)
    }

    fn layer_is_generator(&self, index: usize) -> bool {
        self.engine.project()
            .and_then(|p| p.timeline.layers.get(index))
            .map(|l| l.layer_type == manifold_core::types::LayerType::Generator)
            .unwrap_or(false)
    }

    fn is_layer_muted(&self, index: usize) -> bool {
        self.engine.project()
            .and_then(|p| p.timeline.layers.get(index))
            .map(|l| l.is_muted)
            .unwrap_or(false)
    }

    fn project_beats_per_bar(&self) -> u32 {
        self.engine.project()
            .map(|p| p.settings.time_signature_numerator.max(1) as u32)
            .unwrap_or(4)
    }

    fn get_seconds_per_beat(&self) -> f32 {
        let bpm = self.engine.project()
            .map(|p| p.settings.bpm)
            .unwrap_or(120.0);
        if bpm > 0.0 { 60.0 / bpm } else { 0.5 }
    }

    fn is_playing(&self) -> bool {
        self.engine.is_playing()
    }

    // ── Clip queries ────────────────────────────────────────────

    fn find_clip_by_id(&self, clip_id: &str) -> Option<ClipRef> {
        let project = self.engine.project()?;
        for (li, layer) in project.timeline.layers.iter().enumerate() {
            for clip in &layer.clips {
                if clip.id == clip_id {
                    return Some(ClipRef {
                        clip_id: clip.id.clone(),
                        start_beat: clip.start_beat,
                        duration_beats: clip.duration_beats,
                        end_beat: clip.start_beat + clip.duration_beats,
                        layer_index: li,
                        in_point: clip.in_point,
                        is_generator: layer.layer_type == manifold_core::types::LayerType::Generator,
                        is_locked: clip.is_locked,
                        is_looping: clip.is_looping,
                    });
                }
            }
        }
        None
    }

    // ── Coordinate conversion ───────────────────────────────────

    fn screen_position_to_beat(&self, _pos: Vec2) -> f32 {
        // Delegated to viewport.pixel_to_beat() by the overlay
        0.0
    }

    fn get_layer_index_at_position(&self, _pos: Vec2) -> Option<usize> {
        // Delegated to viewport.layer_at_y() by the overlay
        None
    }

    fn beat_to_time(&self, beat: f32) -> f32 {
        // Simple BPM-based conversion (immutable version).
        // engine.beat_to_timeline_time requires &mut self for tempo map access,
        // but the trait correctly requires &self here.
        let bpm = self.engine.project()
            .map(|p| p.settings.bpm)
            .unwrap_or(120.0);
        if bpm > 0.0 { beat * 60.0 / bpm } else { 0.0 }
    }

    // ── Clip operations ─────────────────────────────────────────

    fn create_clip_at_position(&mut self, beat: f32, layer: usize) -> Option<String> {
        // TODO: wire to EditingService.create_clip_at_position
        // For now, return None (clip creation not yet implemented via this path)
        log::debug!("create_clip_at_position({}, {}) — stub", beat, layer);
        None
    }

    fn move_clip_to_layer(&mut self, clip_id: &str, target_layer: usize) {
        if let Some(project) = self.engine.project_mut() {
            // Find and move the clip
            let mut found = None;
            for (li, layer) in project.timeline.layers.iter().enumerate() {
                if let Some(ci) = layer.clips.iter().position(|c| c.id == clip_id) {
                    found = Some((li, ci));
                    break;
                }
            }
            if let Some((src_layer, clip_idx)) = found {
                if src_layer != target_layer && target_layer < project.timeline.layers.len() {
                    let clip = project.timeline.layers[src_layer].clips.remove(clip_idx);
                    project.timeline.layers[target_layer].clips.push(clip);
                    project.timeline.mark_clip_lookup_dirty();
                }
            }
        }
    }

    // ── Selection & UI ──────────────────────────────────────────

    fn on_clip_selected(&mut self, clip_id: &str) {
        // Find clip's layer and set active_layer
        if let Some(project) = self.engine.project() {
            for (li, layer) in project.timeline.layers.iter().enumerate() {
                if layer.clips.iter().any(|c| c.id == clip_id) {
                    *self.active_layer = Some(li);
                    break;
                }
            }
        }
        *self.needs_structural_sync = true;
    }

    fn on_clip_right_click(&mut self, _clip_id: &str, _screen_pos: Vec2) {
        // Context menu is handled by the UI layer (dropdown panel)
        // The overlay emits this; the app layer opens the dropdown
        log::debug!("on_clip_right_click — routed through overlay");
    }

    fn inspect_layer(&mut self, layer_index: usize) {
        *self.active_layer = Some(layer_index);
        *self.needs_structural_sync = true;
    }

    fn select_region_to(&mut self, _beat: f32, _layer: usize) {
        // Delegated to UIState.select_region_to() by the overlay
        // The overlay calls this on ui_state directly
    }

    // ── Auto-scroll ─────────────────────────────────────────────

    fn auto_scroll_for_drag(&mut self, _screen_pos: Vec2) {
        // Auto-scroll is handled in the app.rs frame loop (existing drag polling)
        // The overlay calls this but the actual scroll logic remains in tick_and_render
    }

    // ── Bitmap invalidation ─────────────────────────────────────

    fn invalidate_layer_bitmap(&mut self, _layer_index: usize) {
        // TODO: wire to bitmap renderer force_dirty per layer
        *self.needs_scroll_rebuild = true;
    }

    fn invalidate_all_layer_bitmaps(&mut self) {
        *self.needs_scroll_rebuild = true;
    }

    fn mark_dirty(&mut self) {
        *self.needs_rebuild = true;
        *self.needs_structural_sync = true;
    }

    // ── Cursor ──────────────────────────────────────────────────

    fn set_cursor(&mut self, cursor: TimelineCursor) {
        let ui_cursor = match cursor {
            TimelineCursor::Default => UICursor::Default,
            TimelineCursor::Move => UICursor::Move,
            TimelineCursor::ResizeHorizontal => UICursor::ResizeHorizontal,
            TimelineCursor::Blocked => UICursor::Blocked,
        };
        self.cursor_manager.set(ui_cursor);
    }

    // ── Playback ────────────────────────────────────────────────

    fn scrub_to_time(&mut self, time: f32) {
        self.engine.seek_to(time);
    }

    // ── Overlap enforcement ─────────────────────────────────────

    fn enforce_non_overlap(&mut self, clip_id: &str, _ignore_ids: &HashSet<String>) {
        // TODO: wire to EditingService.enforce_non_overlap
        // Commands get added to self.command_batch
        log::debug!("enforce_non_overlap({}) — stub", clip_id);
    }

    // ── Region-partial move ─────────────────────────────────────

    fn split_clips_for_region_move(&mut self, _region: &SelectionRegion) -> RegionSplitResult {
        // TODO: wire to EditingService.split_clips_for_region_move
        log::debug!("split_clips_for_region_move — stub");
        RegionSplitResult {
            interior_clip_ids: Vec::new(),
            split_count: 0,
        }
    }

    // ── Command batching ────────────────────────────────────────

    fn begin_command_batch(&mut self) {
        self.command_batch.clear();
    }

    fn record_move(
        &mut self,
        clip_id: &str,
        old_start: f32, new_start: f32,
        old_layer: usize, new_layer: usize,
    ) {
        let cmd = manifold_editing::commands::clip::MoveClipCommand::new(
            clip_id.to_string(),
            old_start, new_start,
            old_layer as i32, new_layer as i32,
        );
        self.command_batch.push(Box::new(cmd));
    }

    fn record_trim(
        &mut self,
        clip_id: &str,
        old_start: f32, new_start: f32,
        old_duration: f32, new_duration: f32,
        old_in_point: f32, new_in_point: f32,
    ) {
        let cmd = manifold_editing::commands::clip::TrimClipCommand::new(
            clip_id.to_string(),
            old_start, new_start,
            old_duration, new_duration,
            old_in_point, new_in_point,
        );
        self.command_batch.push(Box::new(cmd));
    }

    fn commit_command_batch(&mut self, description: &str) {
        if self.command_batch.is_empty() {
            return;
        }
        let commands: Vec<Box<dyn Command>> = self.command_batch.drain(..).collect();
        // Commands are already applied (drag mutated data live).
        // Use record() not execute() — just push to undo stack.
        if commands.len() == 1 {
            let cmd = commands.into_iter().next().unwrap();
            self.editing.record(cmd);
        } else {
            let composite = CompositeCommand::new(commands, description.to_string());
            self.editing.record(Box::new(composite));
        }
        *self.needs_structural_sync = true;
    }

    // ── Live clip mutation ──────────────────────────────────────

    fn set_clip_start_beat(&mut self, clip_id: &str, beat: f32) {
        if let Some(project) = self.engine.project_mut() {
            if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                clip.start_beat = beat;
            }
            project.timeline.mark_clip_lookup_dirty();
        }
    }

    fn set_clip_trim(&mut self, clip_id: &str, start_beat: f32, duration_beats: f32, in_point: f32) {
        if let Some(project) = self.engine.project_mut() {
            if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                clip.start_beat = start_beat;
                clip.duration_beats = duration_beats;
                clip.in_point = in_point;
            }
            project.timeline.mark_clip_lookup_dirty();
        }
    }

    // ── Video metadata ──────────────────────────────────────────

    fn get_max_duration_beats(&self, clip_id: &str) -> f32 {
        // TODO: wire to video library metadata
        // Returns max clip duration based on source video length minus InPoint
        0.0
    }
}
