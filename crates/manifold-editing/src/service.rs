use crate::command::{Command, CompositeCommand};
use crate::undo::UndoRedoManager;
use crate::commands::clip::*;
use manifold_core::clip::TimelineClip;
use manifold_core::project::Project;
use manifold_core::selection::SelectionRegion;
use std::collections::HashSet;

/// Host trait for EditingService — replaces C#'s UIState/CoordinateMapper/PlaybackController.
pub trait EditingHost {
    fn current_beat(&self) -> f32;
    fn seconds_per_beat(&self) -> f32;
    fn grid_interval_beats(&self) -> f32;
    fn floor_beat_to_grid(&self, beat: f32) -> f32;
    fn snap_beat_to_grid(&self, beat: f32) -> f32;
    fn request_clip_sync(&mut self);
    fn mark_compositor_dirty(&mut self);
}

/// Clipboard entry for copy/paste.
#[derive(Debug, Clone)]
struct ClipboardEntry {
    source_clip: TimelineClip,
    beat_offset: f32,
    layer_offset: i32,
}

/// Result of a paste operation.
pub struct PasteResult {
    pub pasted_clip_ids: Vec<String>,
    pub commands: Vec<Box<dyn Command>>,
}

/// Sole mutation gateway for timeline editing operations.
/// Port of C# EditingService.
pub struct EditingService {
    undo_manager: UndoRedoManager,
    clipboard: Vec<ClipboardEntry>,
    data_version: u64,
    saved_at_version: u64,
}

impl EditingService {
    pub fn new() -> Self {
        Self {
            undo_manager: UndoRedoManager::new(),
            clipboard: Vec::new(),
            data_version: 0,
            saved_at_version: 0,
        }
    }

    // ─── Mutation gateway ───

    /// Execute a command through the undo system.
    pub fn execute(&mut self, command: Box<dyn Command>, project: &mut Project) {
        self.undo_manager.execute(command, project);
        self.data_version += 1;
    }

    /// Record an already-executed command (e.g., end of drag).
    pub fn record(&mut self, command: Box<dyn Command>) {
        self.undo_manager.record(command);
        self.data_version += 1;
    }

    /// Undo the most recent command.
    pub fn undo(&mut self, project: &mut Project) -> bool {
        if self.undo_manager.undo(project) {
            self.data_version += 1;
            true
        } else {
            false
        }
    }

    /// Redo the most recently undone command.
    pub fn redo(&mut self, project: &mut Project) -> bool {
        if self.undo_manager.redo(project) {
            self.data_version += 1;
            true
        } else {
            false
        }
    }

    pub fn can_undo(&self) -> bool { self.undo_manager.can_undo() }
    pub fn can_redo(&self) -> bool { self.undo_manager.can_redo() }

    /// Clear undo history (e.g., on project load).
    pub fn set_project(&mut self) {
        self.undo_manager.clear();
        self.data_version = 0;
        self.saved_at_version = 0;
        self.clipboard.clear();
    }

    /// Mark current state as saved.
    pub fn mark_clean(&mut self) {
        self.saved_at_version = self.data_version;
    }

    /// Is there unsaved data?
    pub fn is_dirty(&self) -> bool {
        self.data_version != self.saved_at_version
    }

    pub fn data_version(&self) -> u64 { self.data_version }

    // ─── Clip lookup ───

    pub fn find_clip_by_id<'a>(&self, project: &'a mut Project, clip_id: &str) -> Option<&'a TimelineClip> {
        project.timeline.find_clip_by_id(clip_id)
    }

    // ─── Selection helpers ───

    /// Get clips in a selection region.
    pub fn get_clips_in_region(project: &Project, region: &SelectionRegion) -> Vec<(usize, String)> {
        if !region.is_active {
            return Vec::new();
        }
        let (min_layer, max_layer) = region.layer_range();
        let mut results = Vec::new();

        for (li, layer) in project.timeline.layers.iter().enumerate() {
            let li32 = li as i32;
            if li32 < min_layer || li32 > max_layer {
                continue;
            }
            for clip in &layer.clips {
                // Clip overlaps region if its range intersects
                if clip.start_beat < region.end_beat && clip.end_beat() > region.start_beat {
                    results.push((li, clip.id.clone()));
                }
            }
        }
        results
    }

    // ─── Overlap enforcement ───

    /// Enforce non-overlapping clips on a layer.
    /// Returns commands that fix overlaps caused by `placed_clip`.
    pub fn enforce_non_overlap(
        project: &Project,
        placed_clip: &TimelineClip,
        layer_index: usize,
        ignore_ids: &HashSet<String>,
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();
        let layer = match project.timeline.layers.get(layer_index) {
            Some(l) => l,
            None => return commands,
        };

        let placed_start = placed_clip.start_beat;
        let placed_end = placed_clip.end_beat();

        for clip in &layer.clips {
            if clip.id == placed_clip.id || ignore_ids.contains(&clip.id) {
                continue;
            }

            let clip_start = clip.start_beat;
            let clip_end = clip.end_beat();

            // No overlap
            if clip_end <= placed_start || clip_start >= placed_end {
                continue;
            }

            // Case 1: placed clip covers both start and end → delete existing
            if placed_start <= clip_start && placed_end >= clip_end {
                commands.push(Box::new(DeleteClipCommand::new(clip.clone(), layer_index as i32)));
                continue;
            }

            // Case 2: placed clip covers the start → trim start of existing
            if placed_start <= clip_start && placed_end < clip_end {
                let new_start = placed_end;
                let new_duration = clip_end - new_start;
                let in_point_delta = (new_start - clip_start) * (clip.in_point / clip.duration_beats.max(0.001));
                let new_in_point = clip.in_point + in_point_delta;
                commands.push(Box::new(TrimClipCommand::new(
                    clip.id.clone(),
                    clip.start_beat, new_start,
                    clip.duration_beats, new_duration,
                    clip.in_point, new_in_point,
                )));
                continue;
            }

            // Case 3: placed clip covers the end → trim end of existing
            if placed_start > clip_start && placed_end >= clip_end {
                let new_duration = placed_start - clip_start;
                commands.push(Box::new(TrimClipCommand::new(
                    clip.id.clone(),
                    clip.start_beat, clip.start_beat,
                    clip.duration_beats, new_duration,
                    clip.in_point, clip.in_point,
                )));
                continue;
            }

            // Case 4: placed clip is in the middle → trim existing + create tail
            if placed_start > clip_start && placed_end < clip_end {
                let new_duration = placed_start - clip_start;

                // Create tail clip
                let mut tail = clip.clone_with_new_id();
                tail.start_beat = placed_end;
                tail.duration_beats = clip_end - placed_end;
                let total_original = clip.duration_beats;
                if total_original > 0.0 {
                    tail.in_point = clip.in_point + (placed_end - clip_start) / total_original * (clip.duration_beats * (60.0 / 120.0)); // approximate in-point shift
                }

                commands.push(Box::new(TrimClipCommand::new(
                    clip.id.clone(),
                    clip.start_beat, clip.start_beat,
                    clip.duration_beats, new_duration,
                    clip.in_point, clip.in_point,
                )));
                commands.push(Box::new(AddClipCommand::new(tail, layer_index as i32)));
            }
        }

        commands
    }

    // ─── Clipboard ───

    /// Copy clips to the clipboard.
    pub fn copy_clips(&mut self, project: &Project, clip_ids: &[String]) {
        self.clipboard.clear();
        if clip_ids.is_empty() {
            return;
        }

        // Find all clips and compute offsets relative to the earliest
        let mut clips: Vec<TimelineClip> = Vec::new();
        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if clip_ids.contains(&clip.id) {
                    clips.push(clip.clone());
                }
            }
        }

        if clips.is_empty() {
            return;
        }

        // Find earliest beat and lowest layer
        let min_beat = clips.iter().map(|c| c.start_beat).fold(f32::MAX, f32::min);
        let min_layer = clips.iter().map(|c| c.layer_index).min().unwrap_or(0);

        for clip in clips {
            self.clipboard.push(ClipboardEntry {
                beat_offset: clip.start_beat - min_beat,
                layer_offset: clip.layer_index - min_layer,
                source_clip: clip,
            });
        }
    }

    /// Paste clips from clipboard at the given position.
    pub fn paste_clips(
        &self,
        project: &mut Project,
        target_beat: f32,
        target_layer: i32,
    ) -> PasteResult {
        let mut pasted_ids = Vec::new();
        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        for entry in &self.clipboard {
            let paste_beat = target_beat + entry.beat_offset;
            let paste_layer = (target_layer + entry.layer_offset) as usize;

            // Ensure layer exists
            project.timeline.ensure_layer_count(paste_layer + 1);

            let mut new_clip = entry.source_clip.clone_with_new_id();
            new_clip.start_beat = paste_beat;
            new_clip.layer_index = paste_layer as i32;

            pasted_ids.push(new_clip.id.clone());
            commands.push(Box::new(AddClipCommand::new(new_clip, paste_layer as i32)));
        }

        PasteResult {
            pasted_clip_ids: pasted_ids,
            commands,
        }
    }

    /// Has clipboard content?
    pub fn has_clipboard(&self) -> bool {
        !self.clipboard.is_empty()
    }

    // ─── Region helpers ───

    /// Split a clip at a given beat, returning the command (if split point is valid).
    pub fn split_clip_at_beat(
        project: &Project,
        clip_id: &str,
        split_beat: f32,
    ) -> Option<Box<dyn Command>> {
        // Find the clip
        for (li, layer) in project.timeline.layers.iter().enumerate() {
            if let Some(clip) = layer.find_clip(clip_id) {
                if split_beat <= clip.start_beat || split_beat >= clip.end_beat() {
                    return None;
                }

                let new_duration = split_beat - clip.start_beat;
                let tail_start = split_beat;
                let tail_duration = clip.end_beat() - split_beat;

                let mut tail = clip.clone_with_new_id();
                tail.start_beat = tail_start;
                tail.duration_beats = tail_duration;
                // Adjust in-point for video clips
                if !clip.is_generator() && clip.duration_beats > 0.0 {
                    let spb = 60.0 / 120.0; // TODO: get actual spb from host
                    tail.in_point = clip.in_point + new_duration * spb;
                }
                tail.layer_index = li as i32;

                return Some(Box::new(SplitClipCommand::new(
                    clip.id.clone(),
                    li as i32,
                    clip.duration_beats,
                    new_duration,
                    tail,
                )));
            }
        }
        None
    }

    // ─── Create clip ───

    /// Create a new clip at the given beat and layer.
    pub fn create_clip_at_position(
        project: &mut Project,
        beat: f32,
        layer_index: usize,
        duration_beats: f32,
    ) -> Box<dyn Command> {
        let layer = project.timeline.layers.get(layer_index);
        let is_generator = layer.is_some_and(|l| l.layer_type == manifold_core::types::LayerType::Generator);

        let clip = if is_generator {
            let gen_type = layer.map_or(manifold_core::types::GeneratorType::None, |l| l.generator_type());
            TimelineClip::new_generator(gen_type, layer_index as i32, beat, duration_beats)
        } else {
            TimelineClip {
                layer_index: layer_index as i32,
                start_beat: beat,
                duration_beats,
                ..Default::default()
            }
        };

        Box::new(AddClipCommand::new(clip, layer_index as i32))
    }

    // ─── Duplicate ───

    /// Duplicate selected clips, shifting them forward by the region duration.
    pub fn duplicate_clips(
        project: &Project,
        clip_ids: &[String],
        region: &SelectionRegion,
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();
        let shift = if region.is_active { region.duration_beats() } else { 1.0 };

        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if clip_ids.contains(&clip.id) {
                    let mut new_clip = clip.clone_with_new_id();
                    new_clip.start_beat += shift;
                    commands.push(Box::new(AddClipCommand::new(new_clip, clip.layer_index)));
                }
            }
        }

        commands
    }

    // ─── Delete ───

    /// Delete selected clips.
    pub fn delete_clips(
        project: &Project,
        clip_ids: &[String],
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        for (li, layer) in project.timeline.layers.iter().enumerate() {
            for clip in &layer.clips {
                if clip_ids.contains(&clip.id) {
                    commands.push(Box::new(DeleteClipCommand::new(clip.clone(), li as i32)));
                }
            }
        }

        commands
    }

    // ─── Nudge ───

    /// Nudge selected clips by a beat delta.
    pub fn nudge_clips(
        project: &Project,
        clip_ids: &[String],
        beat_delta: f32,
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if clip_ids.contains(&clip.id) {
                    let new_start = (clip.start_beat + beat_delta).max(0.0);
                    commands.push(Box::new(MoveClipCommand::new(
                        clip.id.clone(),
                        clip.start_beat, new_start,
                        clip.layer_index, clip.layer_index,
                    )));
                }
            }
        }

        commands
    }

    // ─── Extend/Shrink ───

    /// Extend selected clips by one grid step.
    pub fn extend_clips_by_grid(
        project: &Project,
        clip_ids: &[String],
        grid_step: f32,
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if clip_ids.contains(&clip.id) {
                    let new_duration = (clip.duration_beats + grid_step).max(grid_step);
                    commands.push(Box::new(TrimClipCommand::new(
                        clip.id.clone(),
                        clip.start_beat, clip.start_beat,
                        clip.duration_beats, new_duration,
                        clip.in_point, clip.in_point,
                    )));
                }
            }
        }

        commands
    }

    /// Shrink selected clips by one grid step.
    pub fn shrink_clips_by_grid(
        project: &Project,
        clip_ids: &[String],
        grid_step: f32,
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();
        let min_duration = grid_step.max(0.25); // minimum quarter beat

        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if clip_ids.contains(&clip.id) {
                    let new_duration = (clip.duration_beats - grid_step).max(min_duration);
                    if (new_duration - clip.duration_beats).abs() > 0.001 {
                        commands.push(Box::new(TrimClipCommand::new(
                            clip.id.clone(),
                            clip.start_beat, clip.start_beat,
                            clip.duration_beats, new_duration,
                            clip.in_point, clip.in_point,
                        )));
                    }
                }
            }
        }

        commands
    }

    // ─── Move clip to layer ───

    /// Move a clip to a different layer.
    pub fn move_clip_to_layer(
        project: &Project,
        clip_id: &str,
        new_layer_index: i32,
    ) -> Option<Box<dyn Command>> {
        for layer in &project.timeline.layers {
            if let Some(clip) = layer.find_clip(clip_id) {
                if clip.layer_index == new_layer_index {
                    return None;
                }
                return Some(Box::new(MoveClipCommand::new(
                    clip.id.clone(),
                    clip.start_beat, clip.start_beat,
                    clip.layer_index, new_layer_index,
                )));
            }
        }
        None
    }

    // ─── Batch execution helpers ───

    /// Execute multiple commands as a single composite undo entry.
    pub fn execute_batch(
        &mut self,
        commands: Vec<Box<dyn Command>>,
        description: String,
        project: &mut Project,
    ) {
        if commands.is_empty() {
            return;
        }
        let composite = Box::new(CompositeCommand::new(commands, description));
        self.execute(composite, project);
    }
}

impl Default for EditingService {
    fn default() -> Self {
        Self::new()
    }
}
