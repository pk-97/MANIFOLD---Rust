//! Implementation of TimelineEditingHost for the Application.
//!
//! This bridges the manifold-ui trait (which can't depend on manifold-editing)
//! with the concrete engine, editing service, and command system.
//!
//! The wrapper struct `AppEditingHost` borrows individual Application fields
//! to avoid borrowing the entire Application — this lets the overlay
//! simultaneously borrow ui_root and selection from Application.
use manifold_core::{Beats, ClipId, LayerId, Seconds};
use std::collections::HashSet;

use manifold_core::clip::TimelineClip;
use manifold_core::selection::SelectionRegion;
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::service::EditingService;

use manifold_ui::cursors::{CursorManager, TimelineCursor as UICursor};
use manifold_ui::node::Vec2;
use manifold_ui::panels::PanelAction;
use manifold_ui::timeline_editing_host::{
    ClipRef, RegionSplitResult, TimelineCursor, TimelineEditingHost,
};

/// Wrapper that implements TimelineEditingHost by borrowing Application fields.
///
/// Created on the fly when calling InteractionOverlay methods:
/// ```ignore
/// let mut host = AppEditingHost::new(&mut self.engine, &mut self.editing_service, ...);
/// self.ui_root.overlay.on_pointer_click(pos, ..., &mut host, &mut self.selection, &self.ui_root.viewport);
/// ```
pub struct AppEditingHost<'a> {
    pub project: &'a mut manifold_core::project::Project,
    pub content_tx: &'a crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    pub content_state: &'a crate::content_state::ContentState,
    pub cursor_manager: &'a mut CursorManager,
    pub active_layer: &'a mut Option<LayerId>,
    pub needs_rebuild: &'a mut bool,
    pub needs_structural_sync: &'a mut bool,
    pub needs_scroll_rebuild: &'a mut bool,
    /// Per-layer bitmap invalidation targets. Drained in app_render.rs.
    pub invalidate_layers: &'a mut Vec<usize>,

    // Command batch accumulator (for begin_command_batch / commit_command_batch)
    command_batch: Vec<Box<dyn Command>>,

    // Pre-drag split commands — persists at Application level across host instances.
    // Unity: InteractionOverlay.preDragSplitCommands (lines 69, 430-433).
    // Populated by split_clips_for_region_move, prepended on commit_command_batch.
    pub pre_drag_commands: &'a mut Vec<Box<dyn Command>>,

    /// PanelActions generated during overlay processing (e.g. right-click context menus).
    /// Drained by app.rs after the overlay event loop.
    pub pending_actions: Vec<PanelAction>,
}

impl<'a> AppEditingHost<'a> {
    pub fn new(
        project: &'a mut manifold_core::project::Project,
        content_tx: &'a crossbeam_channel::Sender<crate::content_command::ContentCommand>,
        content_state: &'a crate::content_state::ContentState,
        cursor_manager: &'a mut CursorManager,
        active_layer: &'a mut Option<LayerId>,
        needs_rebuild: &'a mut bool,
        needs_structural_sync: &'a mut bool,
        needs_scroll_rebuild: &'a mut bool,
        invalidate_layers: &'a mut Vec<usize>,
        pre_drag_commands: &'a mut Vec<Box<dyn Command>>,
    ) -> Self {
        Self {
            project,
            content_tx,
            content_state,
            cursor_manager,
            active_layer,
            needs_rebuild,
            needs_structural_sync,
            needs_scroll_rebuild,
            invalidate_layers,
            command_batch: Vec::new(),
            pre_drag_commands,
            pending_actions: Vec::new(),
        }
    }
}

impl TimelineEditingHost for AppEditingHost<'_> {
    // ── Data access ─────────────────────────────────────────────

    fn layer_count(&self) -> usize {
        Some(&*self.project)
            .map(|p| p.timeline.layers.len())
            .unwrap_or(0)
    }

    fn layers(&self) -> &[manifold_core::layer::Layer] {
        &self.project.timeline.layers
    }

    fn layer_id_at_index(&self, index: usize) -> Option<manifold_core::LayerId> {
        self.project
            .timeline
            .layers
            .get(index)
            .map(|l| l.layer_id.clone())
    }

    fn layer_is_generator(&self, index: usize) -> bool {
        Some(&*self.project)
            .and_then(|p| p.timeline.layers.get(index))
            .map(|l| l.layer_type == manifold_core::types::LayerType::Generator)
            .unwrap_or(false)
    }

    fn is_layer_muted(&self, index: usize) -> bool {
        Some(&*self.project)
            .and_then(|p| p.timeline.layers.get(index))
            .map(|l| l.is_muted)
            .unwrap_or(false)
    }

    fn project_beats_per_bar(&self) -> u32 {
        Some(&*self.project)
            .map(|p| p.settings.time_signature_numerator.max(1) as u32)
            .unwrap_or(4)
    }

    fn get_seconds_per_beat(&self) -> f32 {
        let bpm = Some(&*self.project)
            .map(|p| p.settings.bpm.0)
            .unwrap_or(120.0);
        if bpm > 0.0 { 60.0 / bpm } else { 0.5 }
    }

    fn is_playing(&self) -> bool {
        self.content_state.is_playing
    }

    // ── Clip queries ────────────────────────────────────────────

    fn find_clip_by_id(&self, clip_id: &str) -> Option<ClipRef> {
        let project = Some(&*self.project)?;
        for (li, layer) in project.timeline.layers.iter().enumerate() {
            for clip in &layer.clips {
                if clip.id == clip_id {
                    return Some(ClipRef {
                        clip_id: clip.id.clone(),
                        start_beat: clip.start_beat,
                        duration_beats: clip.duration_beats,
                        end_beat: clip.start_beat + clip.duration_beats,
                        layer_index: li,
                        layer_id: layer.layer_id.clone(),
                        in_point: clip.in_point,
                        is_generator: layer.layer_type
                            == manifold_core::types::LayerType::Generator,
                        is_locked: clip.is_locked,
                        is_looping: clip.is_looping,
                    });
                }
            }
        }
        None
    }

    // ── Coordinate conversion ───────────────────────────────────

    fn screen_position_to_beat(&self, _pos: Vec2) -> Beats {
        // Delegated to viewport.pixel_to_beat() by the overlay
        Beats::ZERO
    }

    fn get_layer_index_at_position(&self, _pos: Vec2) -> Option<usize> {
        // Delegated to viewport.layer_at_y() by the overlay
        None
    }

    fn beat_to_time(&self, beat: Beats) -> Seconds {
        // Unity delegates to playbackController.TimelineBeatToTime() which uses
        // the full tempo map. Use the immutable version of beat_to_seconds.
        if let Some(project) = Some(&*self.project) {
            let bpm = project.settings.bpm;
            manifold_core::tempo::TempoMapConverter::beat_to_seconds_immut(
                &project.tempo_map,
                beat,
                bpm,
            )
        } else {
            Seconds::ZERO
        }
    }

    // ── Clip operations ─────────────────────────────────────────

    fn create_clip_at_position(
        &mut self,
        beat: Beats,
        layer: usize,
        grid_step: Beats,
    ) -> Option<ClipId> {
        // Port of Unity EditingService.CreateClipAtPosition.
        // Beat arrives pre-snapped from the overlay. grid_step is the clip duration.
        let min_duration = Beats(0.25); // minimum 1/16th note
        let duration = if grid_step < min_duration {
            min_duration
        } else {
            grid_step
        };

        let clip_id = {
            let project = Some(&mut *self.project)?;
            let spb = 60.0 / project.settings.bpm.0.max(1.0);
            // AddClipCommand enforces non-overlap internally.
            let (cmd, id) =
                EditingService::create_clip_at_position(project, beat, layer, duration, spb)?;
            {
                let mut cmd = cmd;
                cmd.execute(project);
                crate::content_command::ContentCommand::send(
                    self.content_tx,
                    crate::content_command::ContentCommand::Execute(cmd),
                );
            }
            id
        };
        *self.needs_structural_sync = true;
        Some(clip_id)
    }

    fn move_clip_to_layer(&mut self, clip_id: &str, target_layer: usize) {
        // Live mutation for drag preview — undo is tracked separately via record_move.
        // Port of Unity EditingService.MoveClipToLayer: validates gen↔video type
        // compatibility, blocks group layers, adopts generator type.
        if let Some(project) = Some(&mut *self.project) {
            if target_layer >= project.timeline.layers.len() {
                return;
            }

            // Block group layers
            if project.timeline.layers[target_layer].is_group() {
                return;
            }

            // Find the clip and its source layer
            let mut found = None;
            for (li, layer) in project.timeline.layers.iter().enumerate() {
                if let Some(ci) = layer.clips.iter().position(|c| c.id == clip_id) {
                    found = Some((li, ci));
                    break;
                }
            }

            if let Some((src_layer, clip_idx)) = found {
                if src_layer == target_layer {
                    return;
                }

                // Gen↔video type mismatch: block
                let clip_is_gen = project.timeline.layers[src_layer].clips[clip_idx]
                    .video_clip_id
                    .is_empty();
                let target_is_gen = project.timeline.layers[target_layer].layer_type
                    == manifold_core::types::LayerType::Generator;
                if clip_is_gen != target_is_gen {
                    return;
                }

                // Move clip between layers (overlap handled by drag enforce).
                let clip_id = ClipId::new(clip_id);
                let clip = project.timeline.layers[src_layer].remove_clip(&clip_id);
                if let Some(clip) = clip {
                    project.timeline.layers[target_layer].restore_clip(clip);
                }
                project.timeline.mark_clip_lookup_dirty();
            }
        }
    }

    // ── Selection & UI ──────────────────────────────────────────

    fn on_clip_selected(&mut self, clip_id: &str) {
        // Find clip's layer and set active_layer only if it changed.
        let mut new_layer: Option<LayerId> = None;
        for layer in &self.project.timeline.layers {
            if layer.clips.iter().any(|c| c.id == clip_id) {
                new_layer = Some(layer.layer_id.clone());
                break;
            }
        }
        if new_layer != *self.active_layer {
            *self.active_layer = new_layer;
            *self.needs_structural_sync = true;
        }
    }

    fn on_clip_right_click(&mut self, clip_id: &str, _screen_pos: Vec2) {
        self.pending_actions
            .push(PanelAction::ClipRightClicked(clip_id.to_string()));
    }

    fn on_track_right_click(&mut self, beat: Beats, layer_index: usize, _screen_pos: Vec2) {
        self.pending_actions
            .push(PanelAction::TrackRightClicked(beat.as_f32(), layer_index));
    }

    fn inspect_layer(&mut self, layer_index: usize) {
        let new_layer = self
            .project
            .timeline
            .layers
            .get(layer_index)
            .map(|l| l.layer_id.clone());
        if new_layer != *self.active_layer {
            *self.active_layer = new_layer;
            *self.needs_structural_sync = true;
        }
    }

    // ── Auto-scroll ─────────────────────────────────────────────

    fn auto_scroll_for_drag(&mut self, _screen_pos: Vec2) {
        // Auto-scroll is handled in the app.rs frame loop (existing drag polling)
        // The overlay calls this but the actual scroll logic remains in tick_and_render
    }

    // ── Bitmap invalidation ─────────────────────────────────────

    fn invalidate_layer_bitmap(&mut self, layer_index: usize) {
        self.invalidate_layers.push(layer_index);
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

    fn scrub_to_time(&mut self, time: Seconds) {
        crate::content_command::ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::SeekTo(time),
        );
    }

    // ── Overlap enforcement ─────────────────────────────────────

    fn enforce_non_overlap(&mut self, clip_id: &str, ignore_ids: &HashSet<ClipId>) {
        // Port of Unity InteractionOverlay overlap enforcement during drag.
        // Commands are executed immediately (model consistency) and stored in
        // command_batch for composite undo on commit_command_batch.
        let spb = self.get_seconds_per_beat();
        let overlap_cmds = {
            if let Some(project) = Some(&*self.project) {
                // Linear scan — find_clip_by_id requires &mut for cache healing
                let mut found: Option<(TimelineClip, usize)> = None;
                for (li, layer) in project.timeline.layers.iter().enumerate() {
                    if let Some(clip) = layer.clips.iter().find(|c| c.id == clip_id) {
                        found = Some((clip.clone(), li));
                        break;
                    }
                }
                if let Some((clip_clone, layer_idx)) = found {
                    EditingService::enforce_non_overlap(
                        project,
                        &clip_clone,
                        layer_idx,
                        ignore_ids,
                        spb,
                    )
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        };
        if !overlap_cmds.is_empty()
            && let Some(project) = Some(&mut *self.project)
        {
            // Execute overlap commands immediately for model consistency,
            // then store in batch for composite undo on commit.
            for mut cmd in overlap_cmds {
                cmd.execute(project);
                self.command_batch.push(cmd);
            }
        }
    }

    // ── Region-partial move ─────────────────────────────────────

    fn split_clips_for_region_move(&mut self, region: &SelectionRegion) -> RegionSplitResult {
        // Port of Unity EditingService.SplitClipsForRegionMove.
        // 1. Split clips at region boundaries (executed immediately)
        // 2. Store split commands in pre_drag_commands for composite undo
        //    (Unity: preDragSplitCommands, lines 69, 430-433)
        // 3. Return interior clips (the drag set)
        let spb = self.get_seconds_per_beat();

        // Step 1: Build split commands (immutable borrow)
        let split_cmds = {
            if let Some(project) = Some(&*self.project) {
                EditingService::split_clips_at_region_boundaries(project, region, spb)
            } else {
                return RegionSplitResult {
                    interior_clip_ids: Vec::new(),
                    split_count: 0,
                };
            }
        };
        let split_count = split_cmds.len();

        // Step 2: Execute split commands immediately, store in pre_drag_commands
        // (not command_batch — these persist across host instances and get prepended
        // on commit so CompositeCommand.Undo() reverses them AFTER undoing the move)
        if !split_cmds.is_empty() {
            self.pre_drag_commands.clear();
            if let Some(project) = Some(&mut *self.project) {
                for mut cmd in split_cmds {
                    cmd.execute(project);
                    self.pre_drag_commands.push(cmd);
                }
            }
        }

        // Step 3: Get clips now fully inside region (immutable borrow)
        let interior_clip_ids = if let Some(project) = Some(&*self.project) {
            EditingService::get_clips_in_region(project, region)
                .into_iter()
                .map(|(_, id)| id)
                .collect()
        } else {
            Vec::new()
        };

        RegionSplitResult {
            interior_clip_ids,
            split_count,
        }
    }

    // ── Command batching ────────────────────────────────────────

    fn begin_command_batch(&mut self) {
        self.command_batch.clear();
    }

    fn record_move(
        &mut self,
        clip_id: &str,
        old_start: Beats,
        new_start: Beats,
        old_layer: usize,
        new_layer: usize,
    ) {
        let old_layer_id = self
            .project
            .timeline
            .layers
            .get(old_layer)
            .map(|l| l.layer_id.clone())
            .unwrap_or_default();
        let new_layer_id = self
            .project
            .timeline
            .layers
            .get(new_layer)
            .map(|l| l.layer_id.clone())
            .unwrap_or_default();
        let cmd = manifold_editing::commands::clip::MoveClipCommand::new(
            ClipId::new(clip_id),
            old_start,
            new_start,
            old_layer_id,
            new_layer_id,
        );
        self.command_batch.push(Box::new(cmd));
    }

    fn record_trim(
        &mut self,
        clip_id: &str,
        old_start: Beats,
        new_start: Beats,
        old_duration: Beats,
        new_duration: Beats,
        old_in_point: Seconds,
        new_in_point: Seconds,
    ) {
        let cmd = manifold_editing::commands::clip::TrimClipCommand::new(
            ClipId::new(clip_id),
            old_start,
            new_start,
            old_duration,
            new_duration,
            old_in_point,
            new_in_point,
        );
        self.command_batch.push(Box::new(cmd));
    }

    fn commit_command_batch(&mut self, description: &str) {
        // Unity lines 428-434: prepend pre-drag split commands so
        // CompositeCommand.Undo() reverses them AFTER undoing the move.
        let pre_cmds: Vec<Box<dyn Command>> = self.pre_drag_commands.drain(..).collect();
        let batch_cmds: Vec<Box<dyn Command>> = self.command_batch.drain(..).collect();

        let mut commands = Vec::with_capacity(pre_cmds.len() + batch_cmds.len());
        commands.extend(pre_cmds);
        commands.extend(batch_cmds);

        if commands.is_empty() {
            return;
        }
        // Commands are already applied (drag mutated data live).
        // Use record() not execute() — just push to undo stack.
        if commands.len() == 1 {
            let cmd = commands.into_iter().next().unwrap();
            crate::content_command::ContentCommand::send(
                self.content_tx,
                crate::content_command::ContentCommand::Execute(cmd),
            );
        } else {
            let composite = CompositeCommand::new(commands, description.to_string());
            crate::content_command::ContentCommand::send(
                self.content_tx,
                crate::content_command::ContentCommand::Execute(Box::new(composite)),
            );
        }
        *self.needs_structural_sync = true;
    }

    // ── Live clip mutation ──────────────────────────────────────

    fn set_clip_start_beat(&mut self, clip_id: &str, beat: Beats) {
        if let Some(project) = Some(&mut *self.project) {
            let mut layer_id = None;
            if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                clip.start_beat = beat;
                layer_id = Some(clip.layer_id.clone());
            }
            if let Some(layer_id) = layer_id
                && let Some(layer_idx) = project.timeline.layer_index_for_id(&layer_id)
                && let Some(layer) = project.timeline.layers.get_mut(layer_idx)
            {
                layer.mark_clips_unsorted();
            }
            project.timeline.mark_clip_lookup_dirty();
        }
    }

    fn set_clip_trim(
        &mut self,
        clip_id: &str,
        start_beat: Beats,
        duration_beats: Beats,
        in_point: Seconds,
    ) {
        if let Some(project) = Some(&mut *self.project) {
            let mut layer_id = None;
            let mut old_start_beat = None;
            if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                old_start_beat = Some(clip.start_beat);
                clip.start_beat = start_beat;
                clip.duration_beats = duration_beats;
                clip.in_point = in_point;
                layer_id = Some(clip.layer_id.clone());
            }
            if old_start_beat.is_some_and(|old| old != start_beat)
                && let Some(layer_id) = layer_id
                && let Some(layer_idx) = project.timeline.layer_index_for_id(&layer_id)
                && let Some(layer) = project.timeline.layers.get_mut(layer_idx)
            {
                layer.mark_clips_unsorted();
            }
            project.timeline.mark_clip_lookup_dirty();
        }
    }

    // ── Video metadata ──────────────────────────────────────────

    fn get_max_duration_beats(&self, clip_id: &str) -> Beats {
        // Linear scan — find_clip_by_id requires &mut self (self-healing cache)
        let clip = self
            .project
            .timeline
            .layers
            .iter()
            .flat_map(|l| l.clips.iter())
            .find(|c| c.id.as_ref() == clip_id);

        let clip = match clip {
            Some(c) => c,
            None => return Beats::ZERO,
        };
        if clip.video_clip_id.is_empty() {
            return Beats::ZERO;
        }

        let video_clip = match self
            .project
            .video_library
            .find_clip_by_id(&clip.video_clip_id)
        {
            Some(vc) => vc,
            None => return Beats::ZERO,
        };

        if video_clip.duration <= 0.0 {
            return Beats::ZERO;
        }

        let available_seconds = (video_clip.duration - clip.in_point.as_f32()).max(0.0);
        let spb = self.get_seconds_per_beat();
        if spb > 0.0 {
            Beats(available_seconds as f64 / spb as f64)
        } else {
            Beats::ZERO
        }
    }
}
