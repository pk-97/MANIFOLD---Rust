//! Implementation of TimelineEditingHost for the Application.
//!
//! This bridges the manifold-ui trait (which can't depend on manifold-editing)
//! with the concrete engine, editing service, and command system.
//!
//! The wrapper struct `AppEditingHost` borrows individual Application fields
//! to avoid borrowing the entire Application — this lets the overlay
//! simultaneously borrow ui_root and selection from Application.
use manifold_ui::{EditingAction};
use manifold_core::{Beats, ClipId, GraphTarget, LayerId, Seconds};
use std::collections::HashSet;

use manifold_core::clip::TimelineClip;
use manifold_core::effects::{AutomationPoint, ParamId, SegmentShape};
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::automation::{
    AddAutomationPointCommand, MoveAutomationPointCommand, RemoveAutomationPointCommand,
};
use manifold_editing::service::EditingService;

use manifold_ui::cursors::{CursorManager, TimelineCursor as UICursor};
use manifold_ui::node::Vec2;
use manifold_ui::panels::PanelAction;
use manifold_ui::timeline_editing_host::{
    ClipRef, RegionSplitResult, TimelineCursor, TimelineEditingHost,
};
use manifold_ui::view::{
    SelectionRegion as UiSelectionRegion, UiGraphTarget, UiLayer, UiSegmentShape,
};

/// `UiGraphTarget` (manifold-ui, no manifold-core dep) → `GraphTarget`
/// (manifold-core). Both variants wrap the identical `EffectId`/`LayerId`
/// re-exported from `manifold-foundation` (see `manifold_core::id`'s header
/// comment), so this is a plain clone, never a lookup or fallible resolve.
/// `pub(crate)`: also used by `input_host.rs`'s Delete-key handler.
pub(crate) fn to_graph_target(target: &UiGraphTarget) -> GraphTarget {
    match target {
        UiGraphTarget::Effect(id) => GraphTarget::Effect(id.clone()),
        UiGraphTarget::Generator(id) => GraphTarget::Generator(id.clone()),
    }
}

/// Reverse of [`to_graph_target`] — `GraphTarget` (manifold-core) →
/// `UiGraphTarget` (manifold-ui). Same plain-clone equivalence; used by
/// touch-to-select (P5, `ui_bridge/inspector.rs`'s `ParamSnapshot` handler)
/// to record the chooser's active param in UI-local terms.
pub(crate) fn to_ui_graph_target(target: &GraphTarget) -> UiGraphTarget {
    match target {
        GraphTarget::Effect(id) => UiGraphTarget::Effect(id.clone()),
        GraphTarget::Generator(id) => UiGraphTarget::Generator(id.clone()),
    }
}

/// `UiSegmentShape` (manifold-ui mirror) → `SegmentShape` (manifold-core) —
/// the reverse of `ui_translate::segment_shape_to_ui`.
fn to_segment_shape(shape: UiSegmentShape) -> SegmentShape {
    match shape {
        UiSegmentShape::Linear => SegmentShape::Linear,
        UiSegmentShape::Hold => SegmentShape::Hold,
        UiSegmentShape::Curved(bend) => SegmentShape::Curved(bend),
    }
}

/// `SegmentShape` (manifold-core) → `UiSegmentShape` (manifold-ui mirror) —
/// the reverse of `to_segment_shape` above (a local copy of
/// `ui_translate::segment_shape_to_ui`, which is private to that module;
/// needed here for `automation_lane_points`' full-list read, P4 Unit B).
fn from_segment_shape(shape: SegmentShape) -> UiSegmentShape {
    match shape {
        SegmentShape::Linear => UiSegmentShape::Linear,
        SegmentShape::Hold => UiSegmentShape::Hold,
        SegmentShape::Curved(bend) => UiSegmentShape::Curved(bend),
    }
}

/// Wrapper that implements TimelineEditingHost by borrowing Application fields.
///
/// Created on the fly when calling InteractionOverlay methods:
/// ```ignore
/// let mut host = AppEditingHost::new(&mut self.engine, &mut self.editing_service, ...);
/// self.ws.ui_root.overlay.on_pointer_click(pos, ..., &mut host, &mut self.selection, &self.ws.ui_root.viewport);
/// ```
pub struct AppEditingHost<'a> {
    pub project: &'a mut manifold_core::project::Project,
    pub content_tx: &'a crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    pub content_state: &'a crate::content_state::ContentState,
    pub cursor_manager: &'a mut CursorManager,
    pub active_layer: &'a mut Option<LayerId>,
    pub needs_rebuild: &'a mut bool,
    pub needs_structural_sync: &'a mut bool,
    pub scroll_dirty: &'a mut crate::ui_root::ScrollDirty,
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

    /// UI-local snapshot of the layer list (Phase 5: the `TimelineEditingHost`
    /// trait speaks UI view-models, not engine types). Built once per host
    /// construction from `project.timeline.layers` — per-interaction, not
    /// per-frame, and tiny (one entry per layer).
    ui_layers: Vec<UiLayer>,
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
        scroll_dirty: &'a mut crate::ui_root::ScrollDirty,
        invalidate_layers: &'a mut Vec<usize>,
        pre_drag_commands: &'a mut Vec<Box<dyn Command>>,
    ) -> Self {
        let ui_layers = crate::ui_translate::layers_to_ui(&project.timeline.layers);
        Self {
            project,
            content_tx,
            content_state,
            cursor_manager,
            active_layer,
            needs_rebuild,
            needs_structural_sync,
            scroll_dirty,
            invalidate_layers,
            command_batch: Vec::new(),
            pre_drag_commands,
            pending_actions: Vec::new(),
            ui_layers,
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

    fn layers(&self) -> &[UiLayer] {
        &self.ui_layers
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

    fn clips_on_layer(&self, layer_index: usize) -> Vec<ClipRef> {
        let Some(layer) = Some(&*self.project).and_then(|p| p.timeline.layers.get(layer_index))
        else {
            return Vec::new();
        };
        layer
            .clips
            .iter()
            .map(|clip| ClipRef {
                clip_id: clip.id.clone(),
                start_beat: clip.start_beat,
                duration_beats: clip.duration_beats,
                end_beat: clip.start_beat + clip.duration_beats,
                layer_index,
                layer_id: layer.layer_id.clone(),
                in_point: clip.in_point,
                is_generator: layer.layer_type == manifold_core::types::LayerType::Generator,
                is_locked: clip.is_locked,
                is_looping: clip.is_looping,
            })
            .collect()
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
            .push(PanelAction::Editing(EditingAction::ClipRightClicked(clip_id.to_string())));
    }

    fn on_track_right_click(&mut self, beat: Beats, layer_index: usize, _screen_pos: Vec2) {
        self.pending_actions
            .push(PanelAction::Editing(EditingAction::TrackRightClicked(beat.as_f32(), layer_index)));
    }

    fn on_automation_lane_right_click(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        _screen_pos: Vec2,
    ) {
        self.pending_actions.push(PanelAction::Editing(EditingAction::AutomationLaneRightClicked(
            target.clone(),
            param_id.clone(),
        )));
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

    // ── Bitmap invalidation ─────────────────────────────────────

    fn invalidate_layer_bitmap(&mut self, layer_index: usize) {
        self.invalidate_layers.push(layer_index);
    }

    fn invalidate_all_layer_bitmaps(&mut self) {
        self.scroll_dirty.visual = true;
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

    fn split_clips_for_region_move(&mut self, region: &UiSelectionRegion) -> RegionSplitResult {
        // Port of Unity EditingService.SplitClipsForRegionMove.
        // 1. Split clips at region boundaries (executed immediately)
        // 2. Store split commands in pre_drag_commands for composite undo
        //    (Unity: preDragSplitCommands, lines 69, 430-433)
        // 3. Return interior clips (the drag set)
        let region = crate::ui_translate::selection_region_to_core(region);
        let region = &region;
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

    fn duplicate_clip_to(&mut self, src_clip_id: &str, target_beat: Beats, target_layer: usize) {
        let project = &mut *self.project;
        let spb = 60.0 / project.settings.bpm.0.max(1.0);
        let src_id = ClipId::new(src_clip_id);
        if let Some(mut cmd) =
            EditingService::duplicate_clip_to(project, &src_id, target_beat, target_layer, spb)
        {
            // Apply to the local mirror now (the copy appears immediately) and
            // push into the batch so it commits as part of the move's one undo
            // entry and reaches the content thread via ExecuteBatch.
            cmd.execute(project);
            self.command_batch.push(cmd);
        }
        *self.needs_structural_sync = true;
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

        // Audio clips bound to their decoded file length, warped: the source
        // advances at `warped_spb = spb * warp_ratio` per beat, so the most beats
        // we can show is the remaining file (after in_point) divided by that.
        if clip.is_audio() {
            let file_secs = clip.source_duration.as_f32();
            if file_secs <= 0.0 {
                return Beats::ZERO;
            }
            let available = (file_secs - clip.in_point.as_f32()).max(0.0);
            let warped_spb =
                self.get_seconds_per_beat() * clip.warp_ratio(self.project.settings.bpm.0);
            return if warped_spb > 0.0 {
                Beats(available as f64 / warped_spb as f64)
            } else {
                Beats::ZERO
            };
        }

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

    // ── Automation lane editing ──────────────────────────────────

    fn add_automation_point(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        beat: Beats,
        value: f32,
        shape: UiSegmentShape,
    ) {
        let target = to_graph_target(target);
        let point = AutomationPoint {
            beat,
            value,
            shape: to_segment_shape(shape),
        };
        let mut cmd = AddAutomationPointCommand::new(target, param_id.as_ref(), point);
        cmd.execute(self.project);
        crate::content_command::ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::Execute(Box::new(cmd)),
        );
    }

    fn set_automation_point_preview(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        from_beat: Beats,
        to_beat: Beats,
        to_value: f32,
    ) {
        let target = to_graph_target(target);
        let param_id = param_id.as_ref();
        if let Some(inst) = self.project.preset_instance_mut(&target)
            && let Some(lanes) = inst.automation_lanes.as_mut()
            && let Some(lane) = lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id)
            && let Some(p) = lane
                .points
                .iter_mut()
                .find(|p| p.beat.0 == from_beat.0)
        {
            p.beat = to_beat;
            p.value = to_value;
            lane.points.sort_by(|a, b| {
                a.beat.partial_cmp(&b.beat).unwrap_or(std::cmp::Ordering::Equal)
            });
        }
    }

    fn commit_automation_point_move(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        old: (Beats, f32, UiSegmentShape),
        new: (Beats, f32, UiSegmentShape),
    ) {
        let graph_target = to_graph_target(target);
        let old_point = AutomationPoint {
            beat: old.0,
            value: old.1,
            shape: to_segment_shape(old.2),
        };
        let new_point = AutomationPoint {
            beat: new.0,
            value: new.1,
            shape: to_segment_shape(new.2),
        };
        // Already applied live by `set_automation_point_preview` during the
        // drag — this only registers the undo entry, mirroring
        // `record_move`'s "commands already applied" comment.
        let cmd = MoveAutomationPointCommand::new(graph_target, param_id.as_ref(), old_point, new_point);
        crate::content_command::ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::Execute(Box::new(cmd)),
        );
    }

    fn remove_automation_point(&mut self, target: &UiGraphTarget, param_id: &ParamId, beat: Beats) {
        let graph_target = to_graph_target(target);
        let param_id_str = param_id.as_ref();
        let index = self.project.preset_instance(&graph_target).and_then(|inst| {
            inst.automation_lanes.as_ref().and_then(|lanes| {
                lanes
                    .iter()
                    .find(|l| l.param_id.as_ref() == param_id_str)
                    .and_then(|lane| lane.points.iter().position(|p| p.beat.0 == beat.0))
            })
        });
        let Some(index) = index else {
            return;
        };
        let mut cmd = RemoveAutomationPointCommand::new(graph_target, param_id.as_ref(), index);
        cmd.execute(self.project);
        crate::content_command::ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::Execute(Box::new(cmd)),
        );
    }

    // ── Automation lane editing — segment gestures (P4 Unit B) ───────

    fn set_automation_segment_bend_preview(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        left_beat: Beats,
        bend: f32,
    ) {
        let target = to_graph_target(target);
        let param_id = param_id.as_ref();
        if let Some(inst) = self.project.preset_instance_mut(&target)
            && let Some(lanes) = inst.automation_lanes.as_mut()
            && let Some(lane) = lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id)
            && let Some(p) = lane.points.iter_mut().find(|p| p.beat.0 == left_beat.0)
        {
            p.shape = SegmentShape::Curved(bend);
        }
    }

    fn set_automation_segment_drag_preview(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        left_beat: Beats,
        left_value: f32,
        right_beat: Beats,
        right_value: f32,
    ) {
        let target = to_graph_target(target);
        let param_id = param_id.as_ref();
        if let Some(inst) = self.project.preset_instance_mut(&target)
            && let Some(lanes) = inst.automation_lanes.as_mut()
            && let Some(lane) = lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id)
        {
            if let Some(p) = lane.points.iter_mut().find(|p| p.beat.0 == left_beat.0) {
                p.value = left_value;
            }
            if let Some(p) = lane.points.iter_mut().find(|p| p.beat.0 == right_beat.0) {
                p.value = right_value;
            }
        }
    }

    fn commit_automation_segment_drag(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        left: (Beats, f32, f32, UiSegmentShape),
        right: (Beats, f32, f32, UiSegmentShape),
    ) {
        let graph_target = to_graph_target(target);
        let param_id_str = param_id.as_ref();
        let make = |beat: Beats, old_v: f32, new_v: f32, shape: UiSegmentShape| {
            let shape = to_segment_shape(shape);
            let old_point = AutomationPoint { beat, value: old_v, shape };
            let new_point = AutomationPoint { beat, value: new_v, shape };
            Box::new(MoveAutomationPointCommand::new(
                graph_target.clone(),
                param_id_str,
                old_point,
                new_point,
            )) as Box<dyn Command>
        };
        // Already applied live by `set_automation_segment_drag_preview` during
        // the drag — this only registers the undo entry. `ExecuteBatch`
        // wraps both moves in a `CompositeCommand` on the content thread so
        // they land as ONE undo/redo unit (existing infra — see
        // `EditingService::execute_batch`).
        let commands = vec![
            make(left.0, left.1, left.2, left.3),
            make(right.0, right.1, right.2, right.3),
        ];
        crate::content_command::ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::ExecuteBatch(
                commands,
                "Move Automation Segment".to_string(),
            ),
        );
    }

    // ── Automation lane editing — marquee group move (P4 Unit B) ─────

    fn commit_automation_group_move(
        &mut self,
        moves: Vec<(UiGraphTarget, ParamId, Beats, f32, f32, UiSegmentShape)>,
    ) {
        if moves.is_empty() {
            return;
        }
        let commands: Vec<Box<dyn Command>> = moves
            .into_iter()
            .map(|(target, param_id, beat, old_v, new_v, shape)| {
                let graph_target = to_graph_target(&target);
                let shape = to_segment_shape(shape);
                let old_point = AutomationPoint { beat, value: old_v, shape };
                let new_point = AutomationPoint { beat, value: new_v, shape };
                Box::new(MoveAutomationPointCommand::new(
                    graph_target,
                    param_id.as_ref(),
                    old_point,
                    new_point,
                )) as Box<dyn Command>
            })
            .collect();
        // Already applied live (per-point, via repeated
        // `set_automation_point_preview` calls) — `ExecuteBatch` batches all
        // of them into ONE undo/redo unit (same existing infra as the
        // segment-drag commit above).
        crate::content_command::ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::ExecuteBatch(
                commands,
                "Move Automation Points".to_string(),
            ),
        );
    }

    // ── Automation lane editing — draw/pencil mode (P4 Unit B) ────────

    fn automation_lane_points(
        &self,
        target: &UiGraphTarget,
        param_id: &ParamId,
    ) -> Option<Vec<(Beats, f32, UiSegmentShape)>> {
        let target = to_graph_target(target);
        let param_id = param_id.as_ref();
        let inst = self.project.preset_instance(&target)?;
        let lanes = inst.automation_lanes.as_ref()?;
        let lane = lanes.iter().find(|l| l.param_id.as_ref() == param_id)?;
        Some(
            lane.points
                .iter()
                .map(|p| (p.beat, p.value, from_segment_shape(p.shape)))
                .collect(),
        )
    }

    fn set_automation_draw_preview(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        points: Vec<(Beats, f32, UiSegmentShape)>,
    ) {
        let target = to_graph_target(target);
        let param_id_str = param_id.as_ref();
        let converted: Vec<AutomationPoint> = points
            .into_iter()
            .map(|(beat, value, shape)| AutomationPoint { beat, value, shape: to_segment_shape(shape) })
            .collect();
        if let Some(inst) = self.project.preset_instance_mut(&target) {
            let lanes = inst.automation_lanes.get_or_insert_with(Vec::new);
            match lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id_str) {
                Some(lane) => lane.points = converted,
                None => lanes.push(manifold_core::effects::AutomationLane {
                    param_id: param_id.clone(),
                    enabled: true,
                    points: converted,
                }),
            }
        }
    }

    fn commit_automation_draw_stroke(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        new_points: Vec<(Beats, f32, UiSegmentShape)>,
        old_points: Option<Vec<(Beats, f32, UiSegmentShape)>>,
    ) {
        use manifold_editing::commands::automation::CommitRecordedGestureCommand;

        let graph_target = to_graph_target(target);
        let param_id_str = param_id.as_ref().to_string();
        let convert = |pts: Vec<(Beats, f32, UiSegmentShape)>| -> Vec<AutomationPoint> {
            pts.into_iter()
                .map(|(beat, value, shape)| AutomationPoint { beat, value, shape: to_segment_shape(shape) })
                .collect()
        };
        let new_converted = convert(new_points);
        let old_converted = old_points.map(convert);
        // Already applied live by `set_automation_draw_preview` during the
        // stroke — this only registers the undo entry, reusing the SAME
        // command §5's Automation Arm recording commits with
        // (`CommitRecordedGestureCommand`).
        let cmd = CommitRecordedGestureCommand::new(graph_target, param_id_str, new_converted, old_converted);
        crate::content_command::ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::Execute(Box::new(cmd)),
        );
    }
}
