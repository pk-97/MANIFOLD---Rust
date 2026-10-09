use crate::command::Command;
use crate::service::EditingService;
use manifold_core::audio_clip_detection::AudioClipDetection;
use manifold_core::clip::TimelineClip;
use manifold_core::layer::OverlapAction;
use manifold_core::project::Project;
use manifold_core::tempo::SourceClock;
use manifold_core::{Beats, ClipId, LayerId, Seconds};
use std::collections::HashSet;

/// Build undoable edits that keep detection-generated children anchored to a
/// source clip's media time after the source clip changes. The caller supplies
/// both clocks because the source tempo itself may be part of the edit.
pub fn build_audio_dependent_edits(
    project: &Project,
    old_source: &TimelineClip,
    new_source: &TimelineClip,
    old_clock: &SourceClock<'_>,
    new_clock: &SourceClock<'_>,
) -> Vec<Box<dyn Command>> {
    if !old_source.is_audio() || !new_source.is_audio() {
        return Vec::new();
    }
    let old_window_start = old_clock.source_position(old_source, old_source.start_beat);
    let old_window_end = old_clock.source_position(old_source, old_source.end_beat());
    let new_window_start = new_clock.source_position(new_source, new_source.start_beat);
    let new_window_end = new_clock.source_position(new_source, new_source.end_beat());
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    let mut moved_ids = Vec::new();

    for layer in &project.timeline.layers {
        for child in &layer.clips {
            if child.detection_source.as_ref() != Some(&old_source.id) {
                continue;
            }

            let child_source_start = old_clock.source_position(old_source, child.start_beat);
            let child_source_end = old_clock.source_position(old_source, child.end_beat());
            let source_start = child_source_start.max(old_window_start).max(new_window_start);
            let source_end = child_source_end.min(old_window_end).min(new_window_end);
            let layer_id = layer.layer_id.clone();

            if source_end <= source_start || new_window_end <= new_window_start {
                commands.push(Box::new(DeleteClipCommand::new(
                    child.clone(),
                    layer_id,
                )));
                continue;
            }

            let mut new_start = new_clock.beat_at_source(new_source, source_start);
            let mut new_end = new_clock.beat_at_source(new_source, source_end);
            new_start = new_start.max(new_source.start_beat);
            new_end = new_end.min(new_source.end_beat());
            if new_end <= new_start {
                commands.push(Box::new(DeleteClipCommand::new(
                    child.clone(),
                    layer_id,
                )));
                continue;
            }

            let new_duration = new_end - new_start;
            moved_ids.push(child.id.clone());
            let old_child_start = old_clock.beat_at_source(old_source, source_start);
            let new_in_point = if child.is_source_media() && old_child_start > child.start_beat {
                child.in_point
                    + old_clock.source_seconds(child, child.start_beat, old_child_start)
            } else {
                child.in_point
            };
            if child.is_audio() {
                commands.push(Box::new(RetimeAudioDependentCommand::new(
                    child,
                    new_start,
                    new_duration,
                    new_in_point,
                    layer_id,
                    new_source,
                )));
            } else {
                commands.push(Box::new(TrimClipCommand::new(
                    child.id.clone(),
                    child.start_beat,
                    new_start,
                    child.duration_beats,
                    new_duration,
                    child.in_point,
                    new_in_point,
                )));
            }
        }
    }
    if !moved_ids.is_empty() {
        commands.push(Box::new(ResolveDependentOverlaps {
            moved_ids,
            commands: None,
        }));
    }
    commands
}

/// Resolve collisions after every child has reached its new position. Protect
/// siblings in this edit, as the ordinary multi-clip move command does.
#[derive(Debug)]
struct ResolveDependentOverlaps {
    moved_ids: Vec<ClipId>,
    commands: Option<Vec<Box<dyn Command>>>,
}

impl Command for ResolveDependentOverlaps {
    fn execute(&mut self, project: &mut Project) {
        if let Some(commands) = &mut self.commands {
            for command in commands { command.execute(project); }
            return;
        }
        let protected = self.moved_ids.iter().cloned().collect();
        let mut commands = Vec::new();
        for id in &self.moved_ids {
            let Some((index, clip)) = project.timeline.layers.iter().enumerate()
                .find_map(|(index, layer)| layer.clips.iter().find(|clip| &clip.id == id)
                    .map(|clip| (index, clip.clone()))) else { continue; };
            for mut command in EditingService::enforce_non_overlap(project, &clip, index, &protected) {
                command.execute(project);
                commands.push(command);
            }
        }
        self.commands = Some(commands);
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(commands) = &mut self.commands {
            for command in commands.iter_mut().rev() { command.undo(project); }
        }
    }

    fn description(&self) -> &str { "Resolve Linked Clip Overlaps" }
}

const MOVE_CLIP_SOURCE_UNAVAILABLE: &str = "move clip source layer or clip is unavailable";
const MOVE_CLIP_TARGET_UNAVAILABLE: &str = "move clip destination layer is unavailable";
const MOVE_CLIP_KIND_MISMATCH: &str = "move clip layer kinds are incompatible";

/// Move a clip to a new beat position and/or layer.
/// Matches Unity MoveClipCommand: cross-layer transfer removes from source and adds to target,
/// with layer-kind admission and undo restoring the original container.
#[derive(Debug)]
pub struct MoveClipCommand {
    clip_id: ClipId,
    old_start_beat: Beats,
    new_start_beat: Beats,
    old_layer_id: LayerId,
    new_layer_id: LayerId,
    dependent_commands: Vec<Box<dyn Command>>,
    dependent_prepared: bool,
    applied: bool,
    rejection: Option<&'static str>,
}

impl MoveClipCommand {
    pub fn new(
        clip_id: ClipId,
        old_start_beat: Beats,
        new_start_beat: Beats,
        old_layer_id: LayerId,
        new_layer_id: LayerId,
    ) -> Self {
        Self {
            clip_id,
            old_start_beat,
            new_start_beat,
            old_layer_id,
            new_layer_id,
            dependent_commands: Vec::new(),
            dependent_prepared: false,
            // Commands can be recorded after a live preview already applied
            // them. Keep the historical default so fresh recorded commands
            // remain undoable; execute() sets it false only on rejection.
            applied: true,
            rejection: None,
        }
    }
}

impl Command for MoveClipCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;

        let Some(src_idx) = project.timeline.layer_index_for_id(&self.old_layer_id) else {
            self.rejection = Some(MOVE_CLIP_SOURCE_UNAVAILABLE);
            return;
        };
        let Some(dst_idx) = project.timeline.layer_index_for_id(&self.new_layer_id) else {
            self.rejection = Some(MOVE_CLIP_TARGET_UNAVAILABLE);
            return;
        };
        let Some(source_layer) = project.timeline.layers.get(src_idx) else {
            self.rejection = Some(MOVE_CLIP_SOURCE_UNAVAILABLE);
            return;
        };
        let Some(source_clip) = source_layer.clips.iter().find(|clip| clip.id == self.clip_id) else {
            self.rejection = Some(MOVE_CLIP_SOURCE_UNAVAILABLE);
            return;
        };
        let destination_layer = &project.timeline.layers[dst_idx];
        if !destination_layer
            .layer_type
            .accepts_clips_from(source_layer.layer_type)
        {
            self.rejection = Some(MOVE_CLIP_KIND_MISMATCH);
            return;
        }

        let old_source = (!self.dependent_prepared).then(|| source_clip.clone());
        if self.old_layer_id != self.new_layer_id {
            // Remove clip from source layer.
            let clip = project.timeline.layers[src_idx].remove_clip(&self.clip_id);

            // Restore clip to target layer (overlap handled by batch).
            if let Some(c) = clip {
                let layer = &mut project.timeline.layers[dst_idx];
                layer.restore_clip(c);
            }
        }

        // Update start_beat on the (now in target layer) clip for both
        // same-layer moves and cross-layer transfers.
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.start_beat = self.new_start_beat;
        }
        if let Some(dst_idx) = project.timeline.layer_index_for_id(&self.new_layer_id)
            && let Some(layer) = project.timeline.layers.get_mut(dst_idx)
        {
            layer.mark_clips_unsorted();
        }
        project.timeline.mark_clip_lookup_dirty();

        if !self.dependent_prepared {
            if let Some(old_source) = old_source
                && let Some(new_source) = project.timeline.find_clip_by_id(&self.clip_id).cloned()
            {
                let clock = project.source_clock();
                self.dependent_commands = build_audio_dependent_edits(
                    project,
                    &old_source,
                    &new_source,
                    &clock,
                    &clock,
                );
            }
            self.dependent_prepared = true;
        }
        for command in &mut self.dependent_commands {
            command.execute(project);
        }
        self.applied = true;
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        for command in self.dependent_commands.iter_mut().rev() {
            command.undo(project);
        }
        if self.old_layer_id != self.new_layer_id {
            let src = project.timeline.layer_index_for_id(&self.new_layer_id);
            let dst = project.timeline.layer_index_for_id(&self.old_layer_id);

            // Remove clip from current (new) layer.
            let clip = if let Some(src_idx) = src
                && let Some(layer) = project.timeline.layers.get_mut(src_idx)
            {
                layer.remove_clip(&self.clip_id)
            } else {
                None
            };

            // Restore clip to original layer (restoring known-good state).
            if let Some(c) = clip
                && let Some(dst_idx) = dst
                && let Some(layer) = project.timeline.layers.get_mut(dst_idx)
            {
                layer.restore_clip(c);
            }
        }

        // Restore generator type and start beat.
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.start_beat = self.old_start_beat;
        }

        if let Some(dst_idx) = project.timeline.layer_index_for_id(&self.old_layer_id)
            && let Some(layer) = project.timeline.layers.get_mut(dst_idx)
        {
            layer.mark_clips_unsorted();
        }
        project.timeline.mark_clip_lookup_dirty();
        self.applied = false;
    }

    fn description(&self) -> &str {
        "Move Clip"
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection
    }

    fn was_applied(&self) -> bool {
        self.applied
    }
}

/// Trim a clip (change start beat, duration, and/or in-point).
/// Calls mark_clips_unsorted when StartBeat changes (matches Unity TrimClipCommand).
#[derive(Debug)]
pub struct TrimClipCommand {
    clip_id: ClipId,
    layer_id: Option<LayerId>,
    old_start_beat: Beats,
    new_start_beat: Beats,
    old_duration_beats: Beats,
    new_duration_beats: Beats,
    old_in_point: Seconds,
    new_in_point: Seconds,
    retime_dependents: bool,
    dependent_commands: Vec<Box<dyn Command>>,
    dependent_prepared: bool,
}

impl TrimClipCommand {
    pub fn new(
        clip_id: ClipId,
        old_start_beat: Beats,
        new_start_beat: Beats,
        old_duration_beats: Beats,
        new_duration_beats: Beats,
        old_in_point: Seconds,
        new_in_point: Seconds,
    ) -> Self {
        Self {
            clip_id,
            layer_id: None,
            old_start_beat,
            new_start_beat,
            old_duration_beats,
            new_duration_beats,
            old_in_point,
            new_in_point,
            retime_dependents: true,
            dependent_commands: Vec::new(),
            dependent_prepared: false,
        }
    }

    /// Geometry-only trim for callers that prepare dependent edits with an
    /// explicit old/new source clock, such as project master-tempo changes.
    pub fn new_geometry_only(
        clip_id: ClipId,
        old_start_beat: Beats,
        new_start_beat: Beats,
        old_duration_beats: Beats,
        new_duration_beats: Beats,
        old_in_point: Seconds,
        new_in_point: Seconds,
    ) -> Self {
        let mut command = Self::new(
            clip_id,
            old_start_beat,
            new_start_beat,
            old_duration_beats,
            new_duration_beats,
            old_in_point,
            new_in_point,
        );
        command.retime_dependents = false;
        command
    }
}

impl Command for TrimClipCommand {
    fn execute(&mut self, project: &mut Project) {
        let old_source = if self.retime_dependents && !self.dependent_prepared {
            project.timeline.find_clip_by_id(&self.clip_id).cloned()
        } else {
            None
        };
        // Capture layer_id on first execute for mark_clips_unsorted.
        if self.layer_id.is_none() {
            for layer in &project.timeline.layers {
                if layer.clips.iter().any(|c| c.id == self.clip_id) {
                    self.layer_id = Some(layer.layer_id.clone());
                    break;
                }
            }
        }

        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.start_beat = self.new_start_beat;
            clip.duration_beats = self.new_duration_beats;
            clip.in_point = self.new_in_point;
        }

        if (self.old_start_beat - self.new_start_beat).0.abs() > f64::EPSILON
            && let Some(ref lid) = self.layer_id
            && let Some(li) = project.timeline.layer_index_for_id(lid)
            && let Some(layer) = project.timeline.layers.get_mut(li)
        {
            layer.mark_clips_unsorted();
        }

        if self.retime_dependents && !self.dependent_prepared {
            if let Some(old_source) = old_source
                && let Some(new_source) = project.timeline.find_clip_by_id(&self.clip_id).cloned()
            {
                let clock = project.source_clock();
                self.dependent_commands = build_audio_dependent_edits(
                    project,
                    &old_source,
                    &new_source,
                    &clock,
                    &clock,
                );
            }
            self.dependent_prepared = true;
        }
        if self.retime_dependents {
            for command in &mut self.dependent_commands {
                command.execute(project);
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if self.retime_dependents {
            for command in self.dependent_commands.iter_mut().rev() {
                command.undo(project);
            }
        }
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.start_beat = self.old_start_beat;
            clip.duration_beats = self.old_duration_beats;
            clip.in_point = self.old_in_point;
        }

        if (self.old_start_beat - self.new_start_beat).0.abs() > f64::EPSILON
            && let Some(ref lid) = self.layer_id
            && let Some(li) = project.timeline.layer_index_for_id(lid)
            && let Some(layer) = project.timeline.layers.get_mut(li)
        {
            layer.mark_clips_unsorted();
        }
    }

    fn description(&self) -> &str {
        "Trim Clip"
    }
}

/// Retimed audio child state. Audio stems keep their own source-file offset;
/// only timeline geometry and the inherited source-tempo metadata change.
#[derive(Debug, Clone, Copy)]
struct RetimeAudioState {
    start_beat: Beats,
    duration_beats: Beats,
    in_point: Seconds,
    recorded_bpm: f32,
    audio_warp_enabled: Option<bool>,
    audio_bpm_automatic: bool,
}

#[derive(Debug)]
struct RetimeAudioDependentCommand {
    clip_id: ClipId,
    layer_id: LayerId,
    old_state: RetimeAudioState,
    new_state: RetimeAudioState,
}

impl RetimeAudioDependentCommand {
    fn new(
        child: &TimelineClip,
        new_start: Beats,
        new_duration: Beats,
        new_in_point: Seconds,
        layer_id: LayerId,
        new_source: &TimelineClip,
    ) -> Self {
        Self {
            clip_id: child.id.clone(),
            layer_id,
            old_state: RetimeAudioState {
                start_beat: child.start_beat,
                duration_beats: child.duration_beats,
                in_point: child.in_point,
                recorded_bpm: child.recorded_bpm,
                audio_warp_enabled: child.audio_warp_enabled,
                audio_bpm_automatic: child.audio_bpm_automatic,
            },
            new_state: RetimeAudioState {
                start_beat: new_start,
                duration_beats: new_duration,
                in_point: new_in_point,
                recorded_bpm: new_source.recorded_bpm,
                audio_warp_enabled: new_source.audio_warp_enabled,
                audio_bpm_automatic: new_source.audio_bpm_automatic,
            },
        }
    }

    fn apply(project: &mut Project, clip_id: &ClipId, state: RetimeAudioState) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
            clip.start_beat = state.start_beat;
            clip.duration_beats = state.duration_beats;
            clip.in_point = state.in_point;
            clip.recorded_bpm = state.recorded_bpm;
            clip.audio_warp_enabled = state.audio_warp_enabled;
            clip.audio_bpm_automatic = state.audio_bpm_automatic;
        }
    }
}

impl Command for RetimeAudioDependentCommand {
    fn execute(&mut self, project: &mut Project) {
        Self::apply(project, &self.clip_id, self.new_state);
        if let Some(layer_index) = project.timeline.layer_index_for_id(&self.layer_id)
            && let Some(layer) = project.timeline.layers.get_mut(layer_index)
        {
            layer.mark_clips_unsorted();
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        Self::apply(project, &self.clip_id, self.old_state);
        if let Some(layer_index) = project.timeline.layer_index_for_id(&self.layer_id)
            && let Some(layer) = project.timeline.layers.get_mut(layer_index)
        {
            layer.mark_clips_unsorted();
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn description(&self) -> &str {
        "Retime Audio Dependent"
    }
}

/// Delete a clip from the timeline.
#[derive(Debug)]
pub struct DeleteClipCommand {
    clip: Option<TimelineClip>,
    layer_id: LayerId,
}

impl DeleteClipCommand {
    pub fn new(clip: TimelineClip, layer_id: LayerId) -> Self {
        Self {
            clip: Some(clip),
            layer_id,
        }
    }
}

impl Command for DeleteClipCommand {
    fn execute(&mut self, project: &mut Project) {
        let clip_id = self.clip.as_ref().unwrap().id.clone();
        if let Some(li) = project.timeline.layer_index_for_id(&self.layer_id)
            && let Some(layer) = project.timeline.layers.get_mut(li)
        {
            layer.remove_clip(&clip_id);
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = self.clip.clone() {
            if let Some(li) = project.timeline.layer_index_for_id(&self.layer_id)
                && let Some(layer) = project.timeline.layers.get_mut(li)
            {
                layer.restore_clip(clip);
            }
            project.timeline.mark_clip_lookup_dirty();
        }
    }

    fn description(&self) -> &str {
        "Delete Clip"
    }
}

/// Add a clip to the timeline with automatic overlap enforcement.
/// On execute, trims/deletes existing clips that collide (DaVinci-style).
/// On undo, reverses those overlap actions and removes the clip.
#[derive(Debug)]
pub struct AddClipCommand {
    clip: TimelineClip,
    layer_id: LayerId,
    /// Clips protected from this add's own overlap enforcement pass —
    /// members of the same batch operation (e.g. the drag/nudge selection
    /// that produced this add via an overlap-split tail). Empty for a
    /// standalone add.
    ignore_ids: HashSet<ClipId>,
    /// Overlap actions performed during execute — reversed on undo.
    overlap_actions: Vec<OverlapAction>,
}

impl AddClipCommand {
    pub fn new(clip: TimelineClip, layer_id: LayerId) -> Self {
        Self {
            clip,
            layer_id,
            ignore_ids: HashSet::new(),
            overlap_actions: Vec::new(),
        }
    }

    /// Same as `new`, but protects `ignore_ids` from this add's own overlap
    /// enforcement pass. Use when this add is a tail/member of a larger
    /// batch operation (e.g. an overlap-split tail born from
    /// `EditingService::enforce_non_overlap`) whose other members must
    /// survive even if this clip's geometry would otherwise collide with
    /// them.
    pub fn new_with_ignore_ids(
        clip: TimelineClip,
        layer_id: LayerId,
        ignore_ids: HashSet<ClipId>,
    ) -> Self {
        Self {
            clip,
            layer_id,
            ignore_ids,
            overlap_actions: Vec::new(),
        }
    }
}

impl Command for AddClipCommand {
    fn execute(&mut self, project: &mut Project) {
        let clock = manifold_core::tempo::SourceClock::new(
            &project.tempo_map,
            project.settings.bpm,
            project.recording_provenance.project_bpm(),
        );
        if let Some(li) = project.timeline.layer_index_for_id(&self.layer_id)
            && let Some(layer) = project.timeline.layers.get_mut(li)
        {
            self.overlap_actions = layer.add_clip(self.clip.clone(), &self.ignore_ids, &clock);
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(li) = project.timeline.layer_index_for_id(&self.layer_id)
            && let Some(layer) = project.timeline.layers.get_mut(li)
        {
            // Remove the added clip.
            layer.remove_clip(&self.clip.id);

            // Reverse overlap actions (in reverse order).
            for action in self.overlap_actions.iter().rev() {
                match action {
                    OverlapAction::Deleted(clip) => {
                        layer.restore_clip(clip.clone());
                    }
                    OverlapAction::Trimmed {
                        clip_id,
                        old_start_beat,
                        old_duration_beats,
                        old_in_point,
                    } => {
                        if let Some(c) = layer.find_clip_mut(clip_id) {
                            c.start_beat = *old_start_beat;
                            c.duration_beats = *old_duration_beats;
                            c.in_point = *old_in_point;
                        }
                    }
                    OverlapAction::Split {
                        clip_id,
                        old_duration_beats,
                        tail_clip,
                    } => {
                        // Remove the tail that was added during the split.
                        layer.remove_clip(&tail_clip.id);
                        // Restore original duration.
                        if let Some(c) = layer.find_clip_mut(clip_id) {
                            c.duration_beats = *old_duration_beats;
                        }
                    }
                }
            }
            layer.mark_clips_unsorted();
        }
        self.overlap_actions.clear();
        project.timeline.mark_clip_lookup_dirty();
    }

    fn description(&self) -> &str {
        "Add Clip"
    }
}

/// Swap the video source of a clip.
#[derive(Debug)]
pub struct SwapVideoCommand {
    clip_id: ClipId,
    old_video_clip_id: String,
    new_video_clip_id: String,
    old_in_point: Seconds,
    new_in_point: Seconds,
    old_duration_beats: Beats,
    new_duration_beats: Beats,
}

impl SwapVideoCommand {
    pub fn new(
        clip_id: ClipId,
        old_video_clip_id: String,
        new_video_clip_id: String,
        old_in_point: Seconds,
        new_in_point: Seconds,
        old_duration_beats: Beats,
        new_duration_beats: Beats,
    ) -> Self {
        Self {
            clip_id,
            old_video_clip_id,
            new_video_clip_id,
            old_in_point,
            new_in_point,
            old_duration_beats,
            new_duration_beats,
        }
    }
}

impl Command for SwapVideoCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.video_clip_id = self.new_video_clip_id.clone();
            clip.in_point = self.new_in_point;
            clip.duration_beats = self.new_duration_beats;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.video_clip_id = self.old_video_clip_id.clone();
            clip.in_point = self.old_in_point;
            clip.duration_beats = self.old_duration_beats;
        }
    }

    fn description(&self) -> &str {
        "Swap Video"
    }
}

/// Replace an audio clip's source file. Shaped like `SwapVideoCommand` above, but
/// for audio: swaps `audio_file_path` + `source_duration`, resets `in_point` to
/// zero and clears the source BPM, Warp, and detection metadata (the old
/// song's tempo is a lie about the new file), keeps `start_beat`/`duration_beats`
/// untouched, and keeps the
/// detection **config** (sensitivities/routing/quantize — the user's tuning)
/// while clearing the cached analysis + per-instrument counts (they describe
/// the old audio). Never touches other clips or invokes detection — pairing
/// this with the `detection_source` cleanup composite is the caller's job (see
/// `docs/TIMELINE_INGEST_DESIGN.md` D6).
#[derive(Debug)]
pub struct ReplaceAudioFileCommand {
    clip_id: ClipId,
    old_path: String,
    new_path: String,
    old_source_duration: Seconds,
    new_source_duration: Seconds,
    old_in_point: Seconds,
    old_recorded_bpm: f32,
    old_detection: Option<AudioClipDetection>,
    old_audio_warp_enabled: Option<Option<bool>>,
    old_audio_bpm_automatic: Option<bool>,
}

impl ReplaceAudioFileCommand {
    pub fn new(
        clip_id: ClipId,
        old_path: String,
        new_path: String,
        old_source_duration: Seconds,
        new_source_duration: Seconds,
        old_in_point: Seconds,
        old_recorded_bpm: f32,
        old_detection: Option<AudioClipDetection>,
    ) -> Self {
        Self {
            clip_id,
            old_path,
            new_path,
            old_source_duration,
            new_source_duration,
            old_in_point,
            old_recorded_bpm,
            old_detection,
            old_audio_warp_enabled: None,
            old_audio_bpm_automatic: None,
        }
    }
}

impl Command for ReplaceAudioFileCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            if self.old_audio_warp_enabled.is_none() {
                self.old_audio_warp_enabled = Some(clip.audio_warp_enabled);
                self.old_audio_bpm_automatic = Some(clip.audio_bpm_automatic);
            }
            clip.audio_file_path = self.new_path.clone();
            clip.source_duration = self.new_source_duration;
            clip.in_point = Seconds::ZERO;
            clip.recorded_bpm = 0.0;
            clip.audio_warp_enabled = None;
            clip.audio_bpm_automatic = false;
            // Keep the config (the user's tuning), clear the analysis + counts
            // (they describe the old file). No config yet ⇒ stays None; the
            // next Detect creates one from scratch, same as a fresh clip.
            if let Some(det) = clip.audio_detection.as_mut() {
                det.analysis = None;
                det.last_counts.clear();
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.audio_file_path = self.old_path.clone();
            clip.source_duration = self.old_source_duration;
            clip.in_point = self.old_in_point;
            clip.recorded_bpm = self.old_recorded_bpm;
            clip.audio_warp_enabled = self.old_audio_warp_enabled.flatten();
            clip.audio_bpm_automatic = self.old_audio_bpm_automatic.unwrap_or(false);
            clip.audio_detection = self.old_detection.clone();
        }
    }

    fn description(&self) -> &str {
        "Replace Audio File"
    }
}

/// Slip a clip's in-point without changing timeline position.
#[derive(Debug)]
pub struct SlipClipCommand {
    clip_id: ClipId,
    old_in_point: Seconds,
    new_in_point: Seconds,
}

impl SlipClipCommand {
    pub fn new(clip_id: ClipId, old_in_point: Seconds, new_in_point: Seconds) -> Self {
        Self {
            clip_id,
            old_in_point,
            new_in_point,
        }
    }
}

impl Command for SlipClipCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.in_point = self.new_in_point;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.in_point = self.old_in_point;
        }
    }

    fn description(&self) -> &str {
        "Slip Clip"
    }
}

/// Change clip visual effects (invert, loop, transform).
#[derive(Debug, Clone)]
pub struct ClipEffectsSnapshot {
    pub is_looping: bool,
    pub loop_duration_beats: Beats,
    pub translate_x: f32,
    pub translate_y: f32,
    pub scale: f32,
    pub rotation: f32,
}

#[derive(Debug)]
pub struct ClipEffectsCommand {
    clip_id: ClipId,
    old: ClipEffectsSnapshot,
    new: ClipEffectsSnapshot,
}

impl ClipEffectsCommand {
    pub fn new(clip_id: ClipId, old: ClipEffectsSnapshot, new: ClipEffectsSnapshot) -> Self {
        Self { clip_id, old, new }
    }

    fn apply(clip: &mut TimelineClip, snap: &ClipEffectsSnapshot) {
        clip.is_looping = snap.is_looping;
        clip.loop_duration_beats = snap.loop_duration_beats;
        clip.translate_x = snap.translate_x;
        clip.translate_y = snap.translate_y;
        clip.scale = snap.scale;
        clip.rotation = snap.rotation;
    }
}

impl Command for ClipEffectsCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            Self::apply(clip, &self.new);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            Self::apply(clip, &self.old);
        }
    }

    fn description(&self) -> &str {
        "Change Clip Effects"
    }
}

/// Change clip loop settings.
#[derive(Debug)]
pub struct ChangeClipLoopCommand {
    clip_id: ClipId,
    old_looping: bool,
    new_looping: bool,
    old_loop_duration: Beats,
    new_loop_duration: Beats,
}

impl ChangeClipLoopCommand {
    pub fn new(
        clip_id: ClipId,
        old_looping: bool,
        new_looping: bool,
        old_loop_duration: Beats,
        new_loop_duration: Beats,
    ) -> Self {
        Self {
            clip_id,
            old_looping,
            new_looping,
            old_loop_duration,
            new_loop_duration,
        }
    }
}

impl Command for ChangeClipLoopCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.is_looping = self.new_looping;
            clip.loop_duration_beats = self.new_loop_duration;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.is_looping = self.old_looping;
            clip.loop_duration_beats = self.old_loop_duration;
        }
    }

    fn description(&self) -> &str {
        "Change Clip Loop"
    }
}

/// The semantic form of an audio tempo edit. Manual BPM edits control warp and
/// preserve the source BPM when turning Warp off; detection only fills an
/// unknown/non-manual source BPM; Warp toggles preserve or seed the source BPM.
#[derive(Debug, Clone, Copy)]
enum RecordedBpmEdit {
    Manual(f32),
    Warp(bool),
    Detected(f32),
}

#[derive(Debug, Clone, Copy)]
struct ClipTempoState {
    recorded_bpm: f32,
    audio_warp_enabled: Option<bool>,
    audio_bpm_automatic: bool,
    duration_beats: Beats,
}

/// Change clip recorded BPM or audio Warp state.
#[derive(Debug)]
pub struct ChangeClipRecordedBpmCommand {
    clip_id: ClipId,
    edit: RecordedBpmEdit,
    old_state: Option<ClipTempoState>,
    new_state: Option<ClipTempoState>,
    dependent_commands: Vec<Box<dyn Command>>,
    overlap_commands: Vec<Box<dyn Command>>,
}

impl ChangeClipRecordedBpmCommand {
    /// Manual BPM edit. Positive audio BPM enables Warp; zero disables Warp
    /// while retaining any known source BPM.
    pub fn new(clip_id: ClipId, _old_bpm: f32, new_bpm: f32) -> Self {
        Self {
            clip_id,
            edit: RecordedBpmEdit::Manual(new_bpm),
            old_state: None,
            new_state: None,
            dependent_commands: Vec::new(),
            overlap_commands: Vec::new(),
        }
    }

    /// Toggle audio Warp. Enabling an audio clip with no known source BPM uses
    /// the project BPM as an assumed source tempo that analysis may replace.
    pub fn new_warp(clip_id: ClipId, enabled: bool) -> Self {
        Self {
            clip_id,
            edit: RecordedBpmEdit::Warp(enabled),
            old_state: None,
            new_state: None,
            dependent_commands: Vec::new(),
            overlap_commands: Vec::new(),
        }
    }

    /// Apply a detected source BPM while preserving the clip's effective Warp
    /// state. Known manual BPM values are left untouched.
    pub fn new_detected(clip_id: ClipId, bpm: f32) -> Self {
        Self {
            clip_id,
            edit: RecordedBpmEdit::Detected(bpm),
            old_state: None,
            new_state: None,
            dependent_commands: Vec::new(),
            overlap_commands: Vec::new(),
        }
    }

    fn apply_edit(clip: &mut TimelineClip, edit: RecordedBpmEdit, project_bpm: f32) {
        match edit {
            RecordedBpmEdit::Manual(bpm) => {
                if clip.is_audio() {
                    if bpm.is_finite() && bpm > 0.0 {
                        clip.set_recorded_bpm(bpm);
                        clip.audio_warp_enabled = Some(true);
                        clip.audio_bpm_automatic = false;
                    } else {
                        clip.audio_warp_enabled = Some(false);
                    }
                } else {
                    clip.set_recorded_bpm(bpm);
                }
            }
            RecordedBpmEdit::Warp(enabled) => {
                if !clip.is_audio() {
                    return;
                }
                if enabled && clip.source_bpm_resolved() <= 0.0 {
                    clip.set_recorded_bpm(project_bpm);
                    clip.audio_bpm_automatic = true;
                }
                clip.audio_warp_enabled = Some(enabled);
            }
            RecordedBpmEdit::Detected(bpm) => {
                if !clip.is_audio() {
                    clip.set_recorded_bpm(bpm);
                    return;
                }
                let was_warp_enabled = clip.is_audio_warp_enabled();
                if clip.source_bpm_resolved() > 0.0 && !clip.audio_bpm_automatic {
                    return;
                }
                clip.set_recorded_bpm(bpm);
                clip.audio_bpm_automatic = clip.source_bpm_resolved() > 0.0;
                clip.audio_warp_enabled = Some(was_warp_enabled);
            }
        }
    }

    fn apply_state(project: &mut Project, clip_id: &ClipId, state: ClipTempoState) -> bool {
        let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) else {
            return false;
        };
        clip.recorded_bpm = state.recorded_bpm;
        clip.audio_warp_enabled = state.audio_warp_enabled;
        clip.audio_bpm_automatic = state.audio_bpm_automatic;
        clip.duration_beats = state.duration_beats;
        true
    }
}

impl Command for ChangeClipRecordedBpmCommand {
    fn execute(&mut self, project: &mut Project) {
        if self.old_state.is_none() {
            let Some(old_clip) = project.timeline.find_clip_by_id(&self.clip_id).cloned() else {
                return;
            };
            let old_state = ClipTempoState {
                recorded_bpm: old_clip.recorded_bpm,
                audio_warp_enabled: old_clip.audio_warp_enabled,
                audio_bpm_automatic: old_clip.audio_bpm_automatic,
                duration_beats: old_clip.duration_beats,
            };
            let mut new_clip = old_clip.clone();
            Self::apply_edit(&mut new_clip, self.edit, project.settings.bpm.0);

            // Preserve the exact source span across a tempo-map boundary. The
            // old and new clip states each get their own SourceClock resolution.
            if old_clip.is_audio() {
                let clock = project.source_clock();
                let source_seconds =
                    clock.source_seconds(&old_clip, old_clip.start_beat, old_clip.end_beat());
                let new_duration = clock.beats_for_source(
                    &new_clip,
                    new_clip.start_beat,
                    source_seconds,
                );
                new_clip.set_duration_beats(new_duration);
            }

            let new_state = ClipTempoState {
                recorded_bpm: new_clip.recorded_bpm,
                audio_warp_enabled: new_clip.audio_warp_enabled,
                audio_bpm_automatic: new_clip.audio_bpm_automatic,
                duration_beats: new_clip.duration_beats,
            };
            self.old_state = Some(old_state);
            self.new_state = Some(new_state);

            Self::apply_state(project, &self.clip_id, new_state);
            let clock = project.source_clock();
            self.dependent_commands = build_audio_dependent_edits(
                project,
                &old_clip,
                &new_clip,
                &clock,
                &clock,
            );
            if old_clip.is_audio()
                && let Some(layer_index) = project.timeline.layer_index_for_id(&old_clip.layer_id)
                && let Some(placed_clip) = project.timeline.find_clip_by_id(&self.clip_id).cloned()
            {
                self.overlap_commands = EditingService::enforce_non_overlap(
                    project,
                    &placed_clip,
                    layer_index,
                    &HashSet::new(),
                );
            }
        } else if let Some(new_state) = self.new_state {
            Self::apply_state(project, &self.clip_id, new_state);
        }

        for command in &mut self.dependent_commands {
            command.execute(project);
        }
        for command in &mut self.overlap_commands {
            command.execute(project);
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        for command in self.overlap_commands.iter_mut().rev() {
            command.undo(project);
        }
        for command in self.dependent_commands.iter_mut().rev() {
            command.undo(project);
        }
        if let Some(old_state) = self.old_state {
            Self::apply_state(project, &self.clip_id, old_state);
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn description(&self) -> &str {
        "Change Recorded BPM"
    }
}

/// Split a clip at a given beat, creating a tail clip.
#[derive(Debug)]
pub struct SplitClipCommand {
    clip_id: ClipId,
    layer_id: LayerId,
    old_duration_beats: Beats,
    new_duration_beats: Beats,
    tail_clip: TimelineClip,
}

impl SplitClipCommand {
    pub fn new(
        clip_id: ClipId,
        layer_id: LayerId,
        old_duration_beats: Beats,
        new_duration_beats: Beats,
        tail_clip: TimelineClip,
    ) -> Self {
        Self {
            clip_id,
            layer_id,
            old_duration_beats,
            new_duration_beats,
            tail_clip,
        }
    }

    /// The clip ID of the tail (right) segment created by the split.
    pub fn tail_clip_id(&self) -> &ClipId {
        &self.tail_clip.id
    }
}

impl Command for SplitClipCommand {
    fn execute(&mut self, project: &mut Project) {
        // Trim original
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.duration_beats = self.new_duration_beats;
        }
        // Restore tail (known non-overlapping — it's the remainder of the split).
        if let Some(li) = project.timeline.layer_index_for_id(&self.layer_id)
            && let Some(layer) = project.timeline.layers.get_mut(li)
        {
            layer.restore_clip(self.tail_clip.clone());
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        // Remove tail
        if let Some(li) = project.timeline.layer_index_for_id(&self.layer_id)
            && let Some(layer) = project.timeline.layers.get_mut(li)
        {
            layer.remove_clip(&self.tail_clip.id);
        }
        // Restore original duration
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.duration_beats = self.old_duration_beats;
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn description(&self) -> &str {
        "Split Clip"
    }
}

/// Mute/unmute a clip.
#[derive(Debug)]
pub struct MuteClipCommand {
    clip_id: ClipId,
    old_muted: bool,
    new_muted: bool,
}

impl MuteClipCommand {
    pub fn new(clip_id: ClipId, old_muted: bool, new_muted: bool) -> Self {
        Self {
            clip_id,
            old_muted,
            new_muted,
        }
    }
}

impl Command for MuteClipCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.is_muted = self.new_muted;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.is_muted = self.old_muted;
        }
    }

    fn description(&self) -> &str {
        "Mute Clip"
    }
}

/// Set a per-clip string parameter (e.g. text content for a text generator).
#[derive(Debug)]
pub struct SetClipStringParamCommand {
    clip_id: ClipId,
    key: String,
    old_value: Option<String>,
    new_value: Option<String>,
}

impl SetClipStringParamCommand {
    pub fn new(
        clip_id: ClipId,
        key: String,
        old_value: Option<String>,
        new_value: Option<String>,
    ) -> Self {
        Self {
            clip_id,
            key,
            old_value,
            new_value,
        }
    }

    fn apply(&self, project: &mut Project, value: &Option<String>) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            match value {
                Some(v) => {
                    clip.string_params
                        .get_or_insert_with(Default::default)
                        .insert(self.key.clone(), v.clone());
                }
                None => {
                    if let Some(map) = &mut clip.string_params {
                        map.remove(&self.key);
                        if map.is_empty() {
                            clip.string_params = None;
                        }
                    }
                }
            }
        }
    }
}

impl Command for SetClipStringParamCommand {
    fn graph_admission_clips(&self, clips: &mut Vec<ClipId>) { clips.push(self.clip_id.clone()); }
    fn execute(&mut self, project: &mut Project) {
        self.apply(project, &self.new_value.clone());
    }

    fn undo(&mut self, project: &mut Project) {
        self.apply(project, &self.old_value.clone());
    }

    fn description(&self) -> &str {
        "Set String Param"
    }
}

#[cfg(test)]
mod dependent_edit_tests {
    use super::*;
    use manifold_core::layer::Layer;
    use manifold_core::types::LayerType;
    use manifold_core::units::Seconds;

    fn project_with_source_and_child() -> (Project, ClipId, ClipId, LayerId) {
        let mut project = Project::default();
        project.settings.bpm = manifold_core::units::Bpm(120.0);
        project
            .timeline
            .insert_layer(0, Layer::new("Source".into(), LayerType::Audio, 0));
        project
            .timeline
            .insert_layer(1, Layer::new("Triggers".into(), LayerType::Generator, 1));

        let source = TimelineClip::new_audio(
            "/source.wav".into(),
            Beats::ZERO,
            Beats(4.0),
            Seconds::ZERO,
            Seconds(10.0),
        );
        let source_id = source.id.clone();
        let mut child = TimelineClip::new_generator(Beats(1.0), Beats(1.0));
        child.detection_source = Some(source_id.clone());
        let child_id = child.id.clone();
        let layer_id = project.timeline.layers[0].layer_id.clone();
        project.timeline.layers[0].restore_clip(source);
        project.timeline.layers[1].restore_clip(child);
        project.timeline.rebuild_clip_lookup();
        (project, source_id, child_id, layer_id)
    }

    #[test]
    fn move_retimes_detection_child_and_undo_restores() {
        let (mut project, source_id, child_id, layer_id) = project_with_source_and_child();
        let mut command = MoveClipCommand::new(
            source_id.clone(),
            Beats::ZERO,
            Beats(4.0),
            layer_id.clone(),
            layer_id,
        );
        command.execute(&mut project);
        let child = project.timeline.find_clip_by_id(&child_id).unwrap();
        assert_eq!(child.start_beat, Beats(5.0));
        assert_eq!(child.duration_beats, Beats(1.0));

        command.undo(&mut project);
        let child = project.timeline.find_clip_by_id(&child_id).unwrap();
        assert_eq!(child.start_beat, Beats(1.0));
        assert_eq!(child.duration_beats, Beats(1.0));
    }

    #[test]
    fn helper_removes_out_of_window_child_and_undo_restores_it() {
        let (mut project, source_id, child_id, _) = project_with_source_and_child();
        let old_source = project.timeline.find_clip_by_id(&source_id).unwrap().clone();
        let mut new_source = old_source.clone();
        new_source.in_point = Seconds(2.0);
        new_source.start_beat = Beats(2.0);
        new_source.duration_beats = Beats(2.0);
        let clock = project.source_clock();
        let mut commands = build_audio_dependent_edits(
            &project,
            &old_source,
            &new_source,
            &clock,
            &clock,
        );
        for command in &mut commands {
            command.execute(&mut project);
        }
        assert!(project.timeline.find_clip_by_id(&child_id).is_none());
        for command in commands.iter_mut().rev() {
            command.undo(&mut project);
        }
        assert!(project.timeline.find_clip_by_id(&child_id).is_some());
    }
}
