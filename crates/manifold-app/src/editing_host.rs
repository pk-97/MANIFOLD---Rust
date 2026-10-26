//! Implementation of TimelineEditingHost for the Application.
//!
//! This bridges the manifold-ui trait (which can't depend on manifold-editing)
//! with the concrete engine, editing service, and command system.
//!
//! The wrapper struct `AppEditingHost` borrows individual Application fields
//! to avoid borrowing the entire Application — this lets the overlay
//! simultaneously borrow ui_root and selection from Application.
use manifold_core::change::{ChangeSet, Part};
use manifold_core::{Beats, ClipId, GraphTarget, LayerId, Seconds};
use manifold_ui::EditingAction;

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
        UiGraphTarget::SceneModifier { owner, modifier_id } => GraphTarget::SceneModifier {
            owner: Box::new(to_graph_target(owner)),
            modifier_id: modifier_id.clone(),
        },
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
        GraphTarget::SceneModifier { owner, modifier_id } => UiGraphTarget::SceneModifier {
            owner: Box::new(to_ui_graph_target(owner)),
            modifier_id: modifier_id.clone(),
        },
    }
}

/// `UiSegmentShape` (manifold-ui mirror) → `SegmentShape` (manifold-core) —
/// the reverse of `ui_translate::segment_shape_to_ui`.
fn to_segment_shape(shape: UiSegmentShape) -> SegmentShape {
    match shape {
        UiSegmentShape::Linear => SegmentShape::Linear,
        UiSegmentShape::Hold => SegmentShape::Hold,
        UiSegmentShape::Curved(bend) => SegmentShape::Curved(bend),
        UiSegmentShape::CurvedRange { bend, start, end } => {
            SegmentShape::CurvedRange { bend, start, end }
        }
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
        SegmentShape::CurvedRange { bend, start, end } => {
            UiSegmentShape::CurvedRange { bend, start, end }
        }
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
    /// The local project: committing methods stamp through it; preview
    /// writes record in the gesture preview (UI_INVALIDATION D5, D12).
    pub project: crate::ui_bridge::ProjectRef<'a>,
    pub content_tx: &'a crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    pub content_state: &'a crate::content_state::ContentState,
    pub cursor_manager: &'a mut CursorManager,
    pub active_layer: &'a mut Option<LayerId>,
    /// The main window repaints next frame (UI_INVALIDATION D8).
    pub redraw: &'a mut bool,

    // Command batch accumulator (for begin_command_batch / commit_command_batch)
    command_batch: Vec<Box<dyn Command>>,

    /// PanelActions generated during overlay processing (e.g. right-click context menus).
    /// Drained by app.rs after the overlay event loop.
    pub pending_actions: Vec<PanelAction>,

    /// UI-local snapshot of the layer list (Phase 5: the `TimelineEditingHost`
    /// trait speaks UI view-models, not engine types). Built on first use,
    /// since most input batches never ask for it.
    ui_layers: std::cell::OnceCell<Vec<UiLayer>>,
}

impl<'a> AppEditingHost<'a> {
    /// Clip lookup through the gesture preview: the clip as drawn.
    fn clip_by_id(&self, clip_id: &str) -> Option<crate::gesture_preview::DrawnClip<'_>> {
        self.project.preview().clip(&self.project, clip_id)
    }

    /// The `ClipRef` a drawn clip answers with.
    fn clip_ref(&self, drawn: &crate::gesture_preview::DrawnClip<'_>) -> ClipRef {
        let layer = &self.project.timeline.layers[drawn.layer_index];
        ClipRef {
            clip_id: drawn.clip.id.clone(),
            start_beat: drawn.start,
            duration_beats: drawn.duration,
            end_beat: drawn.end(),
            layer_index: drawn.layer_index,
            layer_id: layer.layer_id.clone(),
            in_point: drawn.in_point,
            is_generator: layer.hosts_generator(),
            is_locked: drawn.clip.is_locked,
            is_looping: drawn.clip.is_looping,
        }
    }

    /// The data version whose snapshot carries a command sent now.
    fn landing_version(&self) -> u64 {
        self.content_state.data_version + 1
    }

    /// Send `commands` as one undo step the content thread runs.
    fn execute_on_content(&mut self, commands: Vec<Box<dyn Command>>, label: &str) {
        crate::content_command::ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::Execute(Box::new(CompositeCommand::new(
                commands,
                label.to_string(),
            ))),
        );
    }

    /// Send the lane record's points to the content thread once per input
    /// event, leaving the content project's authoritative points available to
    /// the undoable commit.
    fn send_automation_preview(&mut self, target: &GraphTarget, param_id: &ParamId) {
        let Some(points) = self
            .project
            .preview()
            .lane_points(target, param_id.as_ref())
        else {
            return;
        };
        crate::content_command::ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::PreviewAutomationLane {
                target: target.clone(),
                param_id: param_id.clone(),
                points: points.to_vec(),
            },
        );
    }

    pub fn new(
        project: crate::ui_bridge::ProjectRef<'a>,
        content_tx: &'a crossbeam_channel::Sender<crate::content_command::ContentCommand>,
        content_state: &'a crate::content_state::ContentState,
        cursor_manager: &'a mut CursorManager,
        active_layer: &'a mut Option<LayerId>,
        redraw: &'a mut bool,
    ) -> Self {
        Self {
            project,
            content_tx,
            content_state,
            cursor_manager,
            active_layer,
            redraw,
            command_batch: Vec::new(),
            pending_actions: Vec::new(),
            ui_layers: std::cell::OnceCell::new(),
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
        self.ui_layers
            .get_or_init(|| crate::ui_translate::layers_to_ui(&self.project.timeline.layers))
    }

    fn layer_id_at_index(&self, index: usize) -> Option<manifold_core::LayerId> {
        self.project
            .timeline
            .layers
            .get(index)
            .map(|l| l.layer_id.clone())
    }

    fn layer_accepts_clips_from(&self, to: usize, from: usize) -> bool {
        let layers = &self.project.timeline.layers;
        match (layers.get(to), layers.get(from)) {
            (Some(to), Some(from)) => to.layer_type.accepts_clips_from(from.layer_type),
            _ => false,
        }
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

    fn clip_source_seconds(&self, clip_id: &str, from: Beats, to: Beats) -> Seconds {
        match self.clip_by_id(clip_id) {
            Some(drawn) => self
                .project
                .source_clock()
                .source_seconds(drawn.clip, from, to),
            None => Seconds::ZERO,
        }
    }

    fn clip_beats_for_source(&self, clip_id: &str, from: Beats, seconds: Seconds) -> Beats {
        match self.clip_by_id(clip_id) {
            Some(drawn) => self
                .project
                .source_clock()
                .beats_for_source(drawn.clip, from, seconds),
            None => Beats::ZERO,
        }
    }

    fn is_playing(&self) -> bool {
        self.content_state.is_playing
    }

    // ── Clip queries ────────────────────────────────────────────

    fn find_clip_by_id(&self, clip_id: &str) -> Option<ClipRef> {
        self.clip_by_id(clip_id).map(|drawn| self.clip_ref(&drawn))
    }

    fn clips_on_layer(&self, layer_index: usize) -> Vec<ClipRef> {
        self.project
            .preview()
            .clips_on_layer(&self.project, layer_index)
            .map(|drawn| self.clip_ref(&drawn))
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

        // AddClipCommand enforces non-overlap internally. The id is made with
        // the command, so the caller selects the clip before it lands. Its
        // layer takes focus now, since the clip is not in the project yet.
        let (cmd, id) =
            EditingService::create_clip_at_position(&self.project, beat, layer, duration)?;
        *self.active_layer = Some(self.project.timeline.layers[layer].layer_id.clone());
        self.project.execute(cmd);
        Some(id)
    }

    fn move_clip_to_layer(&mut self, clip_id: &str, target_layer: usize) {
        // Drag preview: the record gives the clip its new layer. Undo is
        // tracked separately via record_move. Uses the same container-kind
        // admission as paste and committed moves.
        let (project, preview) = self.project.preview_mut();
        let layers = &project.timeline.layers;
        let Some(target) = layers.get(target_layer) else {
            return;
        };
        let Some(drawn) = preview.clip(project, clip_id) else {
            return;
        };
        if drawn.layer_index == target_layer
            || !target
                .layer_type
                .accepts_clips_from(layers[drawn.layer_index].layer_type)
        {
            return;
        }
        let (id, layer_id) = (drawn.clip.id.clone(), target.layer_id.clone());
        let (start, duration, in_point) = (drawn.start, drawn.duration, drawn.in_point);
        preview.place_clip(&id, &layer_id, start, duration, in_point);
    }

    // ── Selection & UI ──────────────────────────────────────────

    fn on_clip_selected(&mut self, clip_id: &str) {
        // The clip's drawn layer becomes active, only if it changed. A clip
        // that is still on its way to the project leaves focus alone.
        let Some(drawn) = self.clip_by_id(clip_id) else {
            return;
        };
        let new_layer = Some(
            self.project.timeline.layers[drawn.layer_index]
                .layer_id
                .clone(),
        );
        if new_layer != *self.active_layer {
            *self.active_layer = new_layer;
        }
    }

    fn on_clip_right_click(&mut self, clip_id: &str, _screen_pos: Vec2) {
        self.pending_actions
            .push(PanelAction::Editing(EditingAction::ClipRightClicked(
                clip_id.to_string(),
            )));
    }

    fn on_track_right_click(&mut self, beat: Beats, layer_index: usize, _screen_pos: Vec2) {
        self.pending_actions
            .push(PanelAction::Editing(EditingAction::TrackRightClicked(
                beat.as_f32(),
                layer_index,
            )));
    }

    fn on_automation_lane_right_click(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        _screen_pos: Vec2,
    ) {
        self.pending_actions.push(PanelAction::Editing(
            EditingAction::AutomationLaneRightClicked(target.clone(), param_id.clone()),
        ));
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
        }
    }

    // ── Repaint ─────────────────────────────────────────────────

    fn redraw(&mut self) {
        *self.redraw = true;
    }

    // ── Cursor ──────────────────────────────────────────────────

    fn set_cursor(&mut self, cursor: TimelineCursor) {
        let ui_cursor = match cursor {
            TimelineCursor::Default => UICursor::Default,
            TimelineCursor::Crosshair => UICursor::Crosshair,
            TimelineCursor::Move => UICursor::Move,
            TimelineCursor::ResizeHorizontal => UICursor::ResizeHorizontal,
            TimelineCursor::ResizeVertical => UICursor::ResizeVertical,
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

    // ── Overlap resolution ──────────────────────────────────────

    fn resolve_overlaps(&mut self, clip_ids: Vec<ClipId>) {
        // One command for the dropped set: it trims or removes the covered
        // neighbours when the batch commits, on the project that runs it.
        if !clip_ids.is_empty() {
            self.command_batch.push(Box::new(
                manifold_editing::commands::clip::ResolveDependentOverlaps::new(clip_ids),
            ));
        }
    }

    // ── Region-partial move ─────────────────────────────────────

    fn split_clips_for_region_move(&mut self, region: &UiSelectionRegion) -> RegionSplitResult {
        // Port of Unity EditingService.SplitClipsForRegionMove.
        // 1. Build the splits from the project and record each one's result
        //    (the shortened clip, the tail) in the gesture preview.
        // 2. Hold the split commands in the preview; they lead the commit so
        //    CompositeCommand.Undo() reverses them after undoing the move
        //    (Unity: preDragSplitCommands).
        // 3. Return the interior clips (the drag set).
        let region = crate::ui_translate::selection_region_to_core(region);
        let (project, preview) = self.project.preview_mut();
        let (splits, interior) =
            EditingService::split_clips_at_region_boundaries_with_interior(project, &region);
        let split_count = splits.len();
        for split in splits {
            preview.record_split(project, split);
        }
        RegionSplitResult {
            interior_clip_ids: interior.into_iter().map(|(_, id)| id).collect(),
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
        // The copy keeps all of the source's data with a fresh id, at the
        // source's drawn duration and in-point (a region move may have
        // shortened it). It joins the batch as the move's one undo entry
        // and is created when the batch commits.
        let Some(target) = self.project.timeline.layers.get(target_layer) else {
            return;
        };
        let target_layer_id = target.layer_id.clone();
        let Some(src) = self.clip_by_id(src_clip_id) else {
            return;
        };
        let mut copy = src.clip.clone_with_new_id();
        copy.start_beat = target_beat;
        copy.duration_beats = src.duration;
        copy.in_point = src.in_point;
        self.command_batch.push(Box::new(
            manifold_editing::commands::clip::AddClipCommand::new(copy, target_layer_id),
        ));
    }

    fn commit_command_batch(&mut self, description: &str) {
        // Unity lines 428-434: prepend pre-drag split commands so
        // CompositeCommand.Undo() reverses them AFTER undoing the move.
        // The gesture ends here, committed or cancelled. What is sent keeps
        // drawing from the record until its snapshot arrives; a gesture that
        // sends nothing clears at once.
        let mut commands = self.project.preview_mut().1.take_splits();
        commands.append(&mut self.command_batch);

        if commands.is_empty() {
            self.project.preview_mut().1.clear_clips();
            return;
        }
        let command: Box<dyn Command> = if commands.len() == 1 {
            commands.remove(0)
        } else {
            Box::new(CompositeCommand::new(commands, description.to_string()))
        };
        crate::content_command::ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::Execute(command),
        );
        let landing = self.landing_version();
        self.project.preview_mut().1.land_clips(landing);
    }

    // ── Live clip mutation ──────────────────────────────────────

    fn set_clip_start_beat(&mut self, clip_id: &str, beat: Beats) {
        let (project, preview) = self.project.preview_mut();
        let Some(drawn) = preview.clip(project, clip_id) else {
            return;
        };
        let (id, duration, in_point) = (drawn.clip.id.clone(), drawn.duration, drawn.in_point);
        let layer_id = project.timeline.layers[drawn.layer_index].layer_id.clone();
        preview.place_clip(&id, &layer_id, beat, duration, in_point);
    }

    fn set_clip_trim(
        &mut self,
        clip_id: &str,
        start_beat: Beats,
        duration_beats: Beats,
        in_point: Seconds,
    ) {
        let (project, preview) = self.project.preview_mut();
        let Some(drawn) = preview.clip(project, clip_id) else {
            return;
        };
        let id = drawn.clip.id.clone();
        let layer_id = project.timeline.layers[drawn.layer_index].layer_id.clone();
        preview.place_clip(&id, &layer_id, start_beat, duration_beats, in_point);
    }

    // ── Video metadata ──────────────────────────────────────────

    fn get_max_duration_beats(&self, clip_id: &str) -> Beats {
        let Some(drawn) = self.clip_by_id(clip_id) else {
            return Beats::ZERO;
        };
        let clip = drawn.clip;

        // The media left after in_point, converted to beats the way playback
        // advances this clip's source.
        let media_seconds = if clip.is_audio() {
            clip.source_duration.0
        } else {
            match self
                .project
                .video_library
                .find_clip_by_id(&clip.video_clip_id)
            {
                Some(vc) => f64::from(vc.duration),
                None => return Beats::ZERO,
            }
        };
        if media_seconds <= 0.0 {
            return Beats::ZERO;
        }
        let available = Seconds((media_seconds - drawn.in_point.0).max(0.0));
        self.project
            .source_clock()
            .beats_for_source(clip, drawn.start, available)
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
        self.project
            .execute(Box::new(AddAutomationPointCommand::new(
                target,
                param_id.as_ref(),
                point,
            )));
    }

    fn insert_automation_point_on_curve(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        beat: Beats,
        outside_shape: UiSegmentShape,
    ) -> Option<f32> {
        let graph_target = to_graph_target(target);
        let param_id_str = param_id.as_ref();
        // The new point takes the lane's own value there; the insert lands
        // with the next snapshot.
        let value = self
            .project
            .preset_instance(&graph_target)
            .and_then(|inst| inst.automation_lanes.as_ref())
            .and_then(|lanes| {
                lanes
                    .iter()
                    .find(|lane| lane.param_id.as_ref() == param_id_str)
            })
            .and_then(|lane| manifold_editing::commands::automation::on_curve_value(lane, beat))?;
        self.project
            .execute(Box::new(AddAutomationPointCommand::new_on_curve(
                graph_target,
                param_id_str,
                beat,
                to_segment_shape(outside_shape),
            )));
        Some(value)
    }

    fn set_automation_point_preview(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        from_beat: Beats,
        from_value: f32,
        to_beat: Beats,
        to_value: f32,
    ) {
        let target = to_graph_target(target);
        let (project, preview) = self.project.preview_mut();
        if let Some(points) = preview.lane_points_mut(project, &target, param_id.as_ref())
            && let Some(p) = points
                .iter_mut()
                .find(|p| p.beat == from_beat && p.value == from_value)
        {
            p.beat = to_beat;
            p.value = to_value;
            points.sort_by(|a, b| {
                a.beat
                    .partial_cmp(&b.beat)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        self.send_automation_preview(&target, param_id);
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
        // The UI draws the draft from its record; content owns the original
        // lane. Execute captures that complete lane for collision-safe undo.
        let cmd =
            MoveAutomationPointCommand::new(graph_target, param_id.as_ref(), old_point, new_point);
        crate::content_command::ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::Execute(Box::new(cmd)),
        );
    }

    fn remove_automation_point(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        beat: Beats,
        value_norm: f32,
    ) {
        let graph_target = to_graph_target(target);
        let param_id_str = param_id.as_ref();
        let index = self
            .project
            .preset_instance(&graph_target)
            .and_then(|inst| {
                let param = inst.params.get(param_id_str)?;
                inst.automation_lanes.as_ref().and_then(|lanes| {
                    lanes
                        .iter()
                        .find(|l| l.param_id.as_ref() == param_id_str)
                        .and_then(|lane| {
                            lane.points.iter().position(|p| {
                                let norm = manifold_ui::slider::BitmapSlider::value_to_normalized(
                                    p.value,
                                    param.slider_span().0,
                                    param.slider_span().1,
                                );
                                p.beat == beat && norm == value_norm
                            })
                        })
                })
            });
        let Some(index) = index else {
            return;
        };
        self.project
            .execute(Box::new(RemoveAutomationPointCommand::new(
                graph_target,
                param_id.as_ref(),
                index,
            )));
    }

    // ── Automation lane editing — segment gestures (P4 Unit B) ───────

    fn set_automation_segment_bend_preview(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        left_beat: Beats,
        left_value: f32,
        bend: f32,
    ) {
        let target = to_graph_target(target);
        let (project, preview) = self.project.preview_mut();
        if let Some(points) = preview.lane_points_mut(project, &target, param_id.as_ref())
            && let Some(p) = points
                .iter_mut()
                .find(|p| p.beat == left_beat && p.value == left_value)
        {
            p.shape = SegmentShape::Curved(bend);
        }
        self.send_automation_preview(&target, param_id);
    }

    fn set_automation_segment_drag_preview(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        left_beat: Beats,
        left_from_value: f32,
        left_value: f32,
        right_beat: Beats,
        right_from_value: f32,
        right_value: f32,
    ) {
        let target = to_graph_target(target);
        let (project, preview) = self.project.preview_mut();
        if let Some(points) = preview.lane_points_mut(project, &target, param_id.as_ref()) {
            if let Some(p) = points
                .iter_mut()
                .find(|p| p.beat == left_beat && p.value == left_from_value)
            {
                p.value = left_value;
            }
            if let Some(p) = points
                .iter_mut()
                .find(|p| p.beat == right_beat && p.value == right_from_value)
            {
                p.value = right_value;
            }
        }
        self.send_automation_preview(&target, param_id);
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
            let old_point = AutomationPoint {
                beat,
                value: old_v,
                shape,
            };
            let new_point = AutomationPoint {
                beat,
                value: new_v,
                shape,
            };
            Box::new(MoveAutomationPointCommand::new(
                graph_target.clone(),
                param_id_str,
                old_point,
                new_point,
            )) as Box<dyn Command>
        };
        // Apply both moves to the authoritative lane as one undo/redo unit.
        // Runtime previews never replace the source these commands snapshot.
        let commands = vec![
            make(left.0, left.1, left.2, left.3),
            make(right.0, right.1, right.2, right.3),
        ];
        self.execute_on_content(commands, "Move Automation Segment");
    }

    // ── Automation lane editing — marquee group move (P4 Unit B) ─────

    fn commit_automation_group_move(
        &mut self,
        moves: Vec<manifold_ui::timeline_editing_host::AutomationPointMove>,
    ) {
        if moves.is_empty() {
            return;
        }
        // Group by lane before constructing commands: sequential point moves
        // would overwrite other selected sources when a phrase overlaps itself.
        type LaneMoves = (
            GraphTarget,
            ParamId,
            Vec<(AutomationPoint, AutomationPoint)>,
        );
        let mut lanes: Vec<LaneMoves> = Vec::new();
        for movement in moves {
            let target = to_graph_target(&movement.target);
            let convert = |p: (Beats, f32, UiSegmentShape)| AutomationPoint {
                beat: p.0,
                value: p.1,
                shape: to_segment_shape(p.2),
            };
            let pair = (convert(movement.old), convert(movement.new));
            if let Some((_, _, points)) = lanes
                .iter_mut()
                .find(|(t, p, _)| *t == target && *p == movement.param_id)
            {
                points.push(pair);
            } else {
                lanes.push((target, movement.param_id, vec![pair]));
            }
        }
        let commands: Vec<Box<dyn Command>> = lanes
            .into_iter()
            .map(|(target, param_id, points)| {
                Box::new(MoveAutomationPointCommand::for_group(
                    target,
                    param_id.as_ref(),
                    points,
                )) as Box<dyn Command>
            })
            .collect();
        self.execute_on_content(commands, "Move Automation Points");
    }

    // ── Automation lane editing — draw/pencil mode (P4 Unit B) ────────

    fn automation_lane_points(
        &self,
        target: &UiGraphTarget,
        param_id: &ParamId,
    ) -> Option<Vec<(Beats, f32, UiSegmentShape)>> {
        let target = to_graph_target(target);
        let param_id = param_id.as_ref();
        let drawn = |points: &[AutomationPoint]| {
            points
                .iter()
                .map(|p| (p.beat, p.value, from_segment_shape(p.shape)))
                .collect()
        };
        if let Some(points) = self.project.preview().lane_points(&target, param_id) {
            return Some(drawn(points));
        }
        let inst = self.project.preset_instance(&target)?;
        let lanes = inst.automation_lanes.as_ref()?;
        let lane = lanes.iter().find(|l| l.param_id.as_ref() == param_id)?;
        Some(drawn(&lane.points))
    }

    fn set_automation_lane_preview(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        points: &[(Beats, f32, UiSegmentShape)],
    ) {
        self.restore_automation_lane_preview(target, param_id, Some(points));
        self.send_automation_preview(&to_graph_target(target), param_id);
    }

    fn clear_automation_previews(&mut self) {
        crate::content_command::ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::ClearAutomationPreviews,
        );
        let landing = self.landing_version();
        let (project, preview) = self.project.preview_mut();
        preview.land_lanes(project, landing);
    }

    fn restore_automation_lane_preview(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        points: Option<&[(Beats, f32, UiSegmentShape)]>,
    ) {
        let target = to_graph_target(target);
        let (project, preview) = self.project.preview_mut();
        let Some(points) = points else {
            // The lane did not exist before the gesture: stop drawing it. A
            // lane vanishing is a shape change; it stamps like a commit.
            if preview.drop_lane(&target, param_id.as_ref()) {
                self.project
                    .preview_shape_changed(ChangeSet::of(&[Part::Automation]));
            }
            return;
        };
        let known = preview.lane_points(&target, param_id.as_ref()).is_some()
            || project
                .preset_instance(&target)
                .and_then(|inst| inst.automation_lanes.as_ref())
                .is_some_and(|lanes| lanes.iter().any(|l| l.param_id == *param_id));
        if project.preset_instance(&target).is_none() {
            return;
        }
        preview.set_lane(
            &target,
            param_id,
            points.iter().map(|&(beat, value, shape)| AutomationPoint {
                beat,
                value,
                shape: to_segment_shape(shape),
            }),
        );
        // A lane appearing is a shape change; it stamps like a commit.
        if !known {
            self.project
                .preview_shape_changed(ChangeSet::of(&[Part::Automation]));
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
                .map(|(beat, value, shape)| AutomationPoint {
                    beat,
                    value,
                    shape: to_segment_shape(shape),
                })
                .collect()
        };
        let new_converted = convert(new_points);
        let old_converted = old_points.map(convert);
        // Install the draft on content using the same undoable lane replacement
        // as recording; the runtime preview left the original lane untouched.
        let cmd = CommitRecordedGestureCommand::new(
            graph_target,
            param_id_str,
            new_converted,
            old_converted,
        );
        crate::content_command::ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::Execute(Box::new(cmd)),
        );
    }
}
