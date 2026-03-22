use crate::command::{Command, CompositeCommand};
use crate::undo::UndoRedoManager;
use crate::commands::clip::*;
use manifold_core::ClipId;
use manifold_core::clip::TimelineClip;
use manifold_core::project::Project;
use manifold_core::selection::SelectionRegion;
use manifold_core::types::LayerType;
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
#[must_use]
pub struct PasteResult {
    pub pasted_clip_ids: Vec<ClipId>,
    pub skipped_count: usize,
    pub skip_reason: Option<String>,
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
    #[must_use]
    pub fn undo(&mut self, project: &mut Project) -> bool {
        if self.undo_manager.undo(project) {
            self.data_version += 1;
            true
        } else {
            false
        }
    }

    /// Redo the most recently undone command.
    #[must_use]
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
    /// Bumps data_version (rather than resetting to 0) so the content thread's
    /// first post-load snapshot exceeds the app's suppress_snapshot_until
    /// threshold — allowing modulation snapshots to flow immediately.
    pub fn set_project(&mut self) {
        self.undo_manager.clear();
        self.data_version += 1;
        self.saved_at_version = self.data_version;
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
    pub fn get_clips_in_region(project: &Project, region: &SelectionRegion) -> Vec<(usize, ClipId)> {
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
    /// `spb` = seconds per beat (60.0 / bpm).
    pub fn enforce_non_overlap(
        project: &Project,
        placed_clip: &TimelineClip,
        layer_index: usize,
        ignore_ids: &HashSet<ClipId>,
        spb: f32,
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

            // Case 1: placed clip covers both start and end -> delete existing
            if placed_start <= clip_start && placed_end >= clip_end {
                commands.push(Box::new(DeleteClipCommand::new(clip.clone(), layer_index as i32)));
                continue;
            }

            // Case 2: placed clip covers the start -> trim start of existing
            if placed_start <= clip_start && placed_end < clip_end {
                let trim_beats = placed_end - clip_start;
                let trim_seconds = trim_beats * spb;
                let new_in_point = clip.in_point + trim_seconds;
                let new_start = placed_end;
                let new_duration = clip.duration_beats - trim_beats;
                commands.push(Box::new(TrimClipCommand::new(
                    clip.id.clone(),
                    clip.start_beat, new_start,
                    clip.duration_beats, new_duration,
                    clip.in_point, new_in_point,
                )));
                continue;
            }

            // Case 3: placed clip covers the end -> trim end of existing
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

            // Case 4: placed clip is in the middle -> trim existing + create tail
            if placed_start > clip_start && placed_end < clip_end {
                let new_duration = placed_start - clip_start;

                let mut tail = clip.clone_with_new_id();
                tail.start_beat = placed_end;
                tail.duration_beats = clip_end - placed_end;
                let beats_elapsed = placed_end - clip_start;
                tail.in_point = clip.in_point + beats_elapsed * spb;

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
    /// Copy clips to clipboard. When `region` is active, clips are trimmed to
    /// region boundaries and offsets are relative to the region origin (matching
    /// Unity CopySelectedClips region mode). Otherwise clips are copied as-is
    /// with offsets relative to the earliest clip.
    pub fn copy_clips(
        &mut self,
        project: &Project,
        clip_ids: &[ClipId],
        region: Option<&SelectionRegion>,
        spb: f32,
    ) {
        self.clipboard.clear();

        // Region mode: trim clips at boundaries, use region origin
        if let Some(region) = region
            && region.is_active {
                // Find overlapping clips in region (matching Unity CopySelectedClips)
                let overlapping: Vec<&TimelineClip> = project.timeline.layers.iter()
                    .flat_map(|l| l.clips.iter())
                    .filter(|c| {
                        c.start_beat < region.end_beat
                            && c.end_beat() > region.start_beat
                            && clip_ids.contains(&c.id)
                    })
                    .collect();

                if overlapping.is_empty() {
                    return;
                }

                let origin_beat = region.start_beat;
                let (min_layer, _) = region.layer_range();

                for clip in overlapping {
                    let trimmed = Self::trim_clip_to_region(clip, region, spb);
                    self.clipboard.push(ClipboardEntry {
                        beat_offset: trimmed.start_beat - origin_beat,
                        layer_offset: trimmed.layer_index - min_layer,
                        source_clip: trimmed,
                    });
                }
                return;
            }

        // Individual mode: copy full clips, earliest-clip origin
        if clip_ids.is_empty() {
            return;
        }

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
    /// Matches Unity PasteClips: skips gen/video type mismatches,
    /// adopts target layer's generator type, enforces non-overlap.
    pub fn paste_clips(
        &self,
        project: &mut Project,
        target_beat: f32,
        target_layer: i32,
        spb: f32,
    ) -> PasteResult {
        let mut pasted_ids = Vec::new();
        let mut commands: Vec<Box<dyn Command>> = Vec::new();
        let mut skipped = 0usize;

        for entry in &self.clipboard {
            let paste_beat = target_beat + entry.beat_offset;
            let paste_layer_idx = (target_layer + entry.layer_offset) as usize;

            // Ensure layer exists
            project.timeline.ensure_layer_count(paste_layer_idx + 1);

            let layer = match project.timeline.layers.get(paste_layer_idx) {
                Some(l) => l,
                None => { skipped += 1; continue; }
            };

            let clip_is_gen = entry.source_clip.is_generator();
            let layer_is_gen = layer.layer_type == LayerType::Generator;

            // Gen<->video mismatch: skip
            if clip_is_gen != layer_is_gen {
                skipped += 1;
                continue;
            }

            let mut new_clip = entry.source_clip.clone_with_new_id();
            new_clip.start_beat = paste_beat;
            new_clip.layer_index = paste_layer_idx as i32;

            // Gen->gen with different type: adopt target layer's generator type
            if clip_is_gen && layer_is_gen && new_clip.generator_type != layer.generator_type() {
                new_clip.generator_type = layer.generator_type();
            }

            // Enforce non-overlap for the new clip
            let empty_ignore = HashSet::new();
            let overlap_cmds = Self::enforce_non_overlap(
                project, &new_clip, paste_layer_idx, &empty_ignore, spb,
            );
            commands.extend(overlap_cmds);

            pasted_ids.push(new_clip.id.clone());
            commands.push(Box::new(AddClipCommand::new(new_clip, paste_layer_idx as i32)));
        }

        PasteResult {
            pasted_clip_ids: pasted_ids,
            skipped_count: skipped,
            skip_reason: if skipped > 0 {
                Some("generator/video type mismatch".to_string())
            } else {
                None
            },
            commands,
        }
    }

    /// Has clipboard content?
    pub fn has_clipboard(&self) -> bool {
        !self.clipboard.is_empty()
    }

    // ─── Region helpers ───

    /// Split a clip at a given beat, returning the command (if split point is valid).
    /// `spb` = seconds per beat (60.0 / bpm).
    pub fn split_clip_at_beat(
        project: &Project,
        clip_id: &str,
        split_beat: f32,
        spb: f32,
    ) -> Option<Box<dyn Command>> {
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
                if !clip.is_generator() && clip.duration_beats > 0.0 {
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

    /// Split clips that straddle region boundaries.
    /// Returns commands for all splits performed.
    /// Matches Unity SplitClipsAtRegionBoundaries.
    pub fn split_clips_at_region_boundaries(
        project: &Project,
        region: &SelectionRegion,
        spb: f32,
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        let (min_layer, max_layer) = region.layer_range();
        let layer_count = project.timeline.layers.len();
        let start_layer = (min_layer.max(0) as usize).min(layer_count.saturating_sub(1));
        let end_layer = (max_layer.max(0) as usize).min(layer_count.saturating_sub(1));

        for li in start_layer..=end_layer {
            // Snapshot clip IDs (splits add new clips)
            let clip_ids: Vec<ClipId> = project.timeline.layers[li]
                .clips.iter().map(|c| c.id.clone()).collect();

            for clip_id in &clip_ids {
                let clip = match project.timeline.layers[li].find_clip(clip_id) {
                    Some(c) => c,
                    None => continue,
                };

                if clip.end_beat() <= region.start_beat || clip.start_beat >= region.end_beat {
                    continue;
                }

                // Split at region end FIRST (so the original's EndBeat is still valid
                // when we split at region start)
                if clip.end_beat() > region.end_beat
                    && let Some(cmd) = Self::split_clip_at_beat(project, clip_id, region.end_beat, spb) {
                        commands.push(cmd);
                    }

                // Split at region start
                if clip.start_beat < region.start_beat
                    && let Some(cmd) = Self::split_clip_at_beat(project, clip_id, region.start_beat, spb) {
                        commands.push(cmd);
                    }
            }
        }

        commands
    }

    // ─── Trim clip to region ───

    /// Create a clone of a clip trimmed to fit within region boundaries.
    /// Does NOT modify the original. Used for region-aware copy/duplicate.
    /// Port of Unity EditingService.TrimClipToRegion.
    pub fn trim_clip_to_region(
        clip: &TimelineClip,
        region: &SelectionRegion,
        spb: f32,
    ) -> TimelineClip {
        let new_start = clip.start_beat.max(region.start_beat);
        let new_end = clip.end_beat().min(region.end_beat);
        let new_duration = (new_end - new_start).max(0.0);

        let mut trimmed = clip.clone_with_new_id();
        trimmed.start_beat = new_start;
        trimmed.duration_beats = new_duration;

        // Adjust in_point for video clips (generators have no in_point)
        if !clip.is_generator() {
            trimmed.in_point = clip.in_point + (new_start - clip.start_beat) * spb;
        }

        trimmed
    }

    // ─── Create clip ───

    /// Create a new clip at the given beat and layer.
    /// Returns (command, clip_id) so the caller can track the new clip.
    pub fn create_clip_at_position(
        project: &mut Project,
        beat: f32,
        layer_index: usize,
        duration_beats: f32,
    ) -> (Box<dyn Command>, ClipId) {
        let layer = project.timeline.layers.get(layer_index);
        let is_generator = layer.is_some_and(|l| l.layer_type == LayerType::Generator);

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

        let clip_id = clip.id.clone();
        (Box::new(AddClipCommand::new(clip, layer_index as i32)), clip_id)
    }

    // ─── Duplicate ───

    /// Duplicate selected clips, shifting them forward.
    /// Region mode: trims clips to region boundaries, places copies after region end.
    /// Individual mode: places copies offset by the selected clips' span.
    /// Matches Unity DuplicateSelectedClips.
    pub fn duplicate_clips(
        project: &Project,
        clip_ids: &[ClipId],
        region: &SelectionRegion,
        spb: f32,
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        if region.is_active {
            // Region mode: find ALL clips overlapping the region (Unity FillClipsInRegion),
            // trim to region boundaries, place copies after region end.
            // The offset is the full region duration, preserving gaps (Ableton behavior).
            let offset = region.duration_beats();
            let start_layer = region.start_layer_index.min(region.end_layer_index).max(0) as usize;
            let end_layer = (region.start_layer_index.max(region.end_layer_index) as usize)
                .min(project.timeline.layers.len().saturating_sub(1));

            for li in start_layer..=end_layer {
                let layer = &project.timeline.layers[li];
                for clip in &layer.clips {
                    // Overlap test: clip intersects region (Unity lines 182-183)
                    if clip.end_beat() <= region.start_beat
                        || clip.start_beat >= region.end_beat
                    {
                        continue;
                    }
                    let trimmed = Self::trim_clip_to_region(clip, region, spb);
                    let mut new_clip = trimmed;
                    new_clip.start_beat += offset;
                    commands.push(Box::new(AddClipCommand::new(new_clip, clip.layer_index)));
                }
            }
        } else {
            // Individual mode: offset by the clips' own span
            let mut min_beat = f32::MAX;
            let mut max_end = f32::MIN;
            for layer in &project.timeline.layers {
                for clip in &layer.clips {
                    if clip_ids.contains(&clip.id) {
                        min_beat = min_beat.min(clip.start_beat);
                        max_end = max_end.max(clip.end_beat());
                    }
                }
            }
            let shift = if max_end > min_beat { max_end - min_beat } else { 1.0 };

            for layer in &project.timeline.layers {
                for clip in &layer.clips {
                    if clip_ids.contains(&clip.id) {
                        let mut new_clip = clip.clone_with_new_id();
                        new_clip.start_beat += shift;
                        commands.push(Box::new(AddClipCommand::new(new_clip, clip.layer_index)));
                    }
                }
            }
        }

        commands
    }

    // ─── Delete ───

    /// Delete selected clips.
    /// When a region is active, splits clips at region boundaries first
    /// then deletes only the interior segments. Matches Unity DeleteSelectedClips.
    pub fn delete_clips(
        project: &Project,
        clip_ids: &[ClipId],
        region: Option<&SelectionRegion>,
        spb: f32,
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        if let Some(region) = region
            && region.is_active {
                // Region mode: split at boundaries, then delete clips inside region
                let split_cmds = Self::split_clips_at_region_boundaries(project, region, spb);
                commands.extend(split_cmds);

                // After splits, collect clips fully inside the region
                let clips_in_region = Self::get_clips_in_region(project, region);
                for (li, clip_id) in &clips_in_region {
                    if let Some(clip) = project.timeline.layers[*li].find_clip(clip_id) {
                        commands.push(Box::new(DeleteClipCommand::new(clip.clone(), *li as i32)));
                    }
                }

                return commands;
            }

        // Individual selection path
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
    /// Enforces non-overlap after nudging, excluding the nudged clips from resolution.
    /// Matches Unity NudgeSelectedClips.
    pub fn nudge_clips(
        project: &Project,
        clip_ids: &[ClipId],
        beat_delta: f32,
        spb: f32,
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();
        let mut nudged_ids: HashSet<ClipId> = HashSet::new();

        // Collect move commands for each nudged clip
        // We build "virtual" post-move clips to compute overlap enforcement
        struct NudgedClip {
            clip: TimelineClip,
            layer_index: usize,
        }
        let mut nudged_clips: Vec<NudgedClip> = Vec::new();

        for (li, layer) in project.timeline.layers.iter().enumerate() {
            for clip in &layer.clips {
                if !clip_ids.contains(&clip.id) {
                    continue;
                }
                let old_start = clip.start_beat;
                let new_start = (old_start + beat_delta).max(0.0);
                if (new_start - old_start).abs() < 0.0001 {
                    continue;
                }

                commands.push(Box::new(MoveClipCommand::new(
                    clip.id.clone(),
                    old_start, new_start,
                    clip.layer_index, clip.layer_index,
                )));
                nudged_ids.insert(clip.id.clone());

                // Build virtual post-move clip for overlap enforcement
                let mut moved_clip = clip.clone();
                moved_clip.start_beat = new_start;
                nudged_clips.push(NudgedClip {
                    clip: moved_clip,
                    layer_index: li,
                });
            }
        }

        if commands.is_empty() {
            return commands;
        }

        // Enforce overlaps for each nudged clip, excluding other nudged clips
        for nudged in &nudged_clips {
            let overlap_cmds = Self::enforce_non_overlap(
                project, &nudged.clip, nudged.layer_index, &nudged_ids, spb,
            );
            commands.extend(overlap_cmds);
        }

        commands
    }

    // ─── Extend/Shrink ───

    /// Extend selected clips by one grid step.
    /// Clamps to prevent overlap with the next clip on the same layer.
    /// Matches Unity ExtendSelectedClipsByGridStep.
    pub fn extend_clips_by_grid(
        project: &Project,
        clip_ids: &[ClipId],
        grid_step: f32,
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if !clip_ids.contains(&clip.id) {
                    continue;
                }

                let mut new_duration = clip.duration_beats + grid_step;

                // Clamp to next clip on same layer to prevent overlap
                let mut next_start = f32::MAX;
                for other in &layer.clips {
                    if other.id == clip.id {
                        continue;
                    }
                    if other.start_beat > clip.start_beat && other.start_beat < next_start {
                        next_start = other.start_beat;
                    }
                }
                let max_duration = next_start - clip.start_beat;
                if new_duration > max_duration {
                    new_duration = max_duration;
                }

                // Skip if duration didn't actually change or decreased
                if (new_duration - clip.duration_beats).abs() < 0.001 {
                    continue;
                }
                if new_duration <= clip.duration_beats {
                    continue;
                }

                commands.push(Box::new(TrimClipCommand::new(
                    clip.id.clone(),
                    clip.start_beat, clip.start_beat,
                    clip.duration_beats, new_duration,
                    clip.in_point, clip.in_point,
                )));
            }
        }

        commands
    }

    /// Shrink selected clips by one grid step.
    pub fn shrink_clips_by_grid(
        project: &Project,
        clip_ids: &[ClipId],
        grid_step: f32,
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();
        let min_duration = 0.25; // Fixed 1/16th note minimum. Port of Unity line 861: const float minDuration = 0.25f;

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
    /// Checks gen/video compatibility and adopts generator type.
    /// Matches Unity MoveClipToLayer.
    pub fn move_clip_to_layer(
        project: &Project,
        clip_id: &str,
        new_layer_index: i32,
    ) -> Option<Box<dyn Command>> {
        let target_idx = new_layer_index as usize;
        let target_layer = project.timeline.layers.get(target_idx)?;

        // Block group layers
        if target_layer.is_group() {
            return None;
        }

        for layer in &project.timeline.layers {
            if let Some(clip) = layer.find_clip(clip_id) {
                if clip.layer_index == new_layer_index {
                    return None;
                }

                // Gen/video type mismatch: block
                let clip_is_gen = clip.is_generator();
                let target_is_gen = target_layer.layer_type == LayerType::Generator;
                if clip_is_gen != target_is_gen {
                    return None;
                }

                // MoveClipCommand handles generator type adoption internally
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

    // ─── Selection operations (Phase 3A) ───
    // Note: These are STATELESS in Rust (caller passes selection data explicitly).
    // Unity's versions read from UIState directly — this is an approved architectural
    // divergence (Rust EditingService is stateless, all state passed as parameters).

    /// Get effective selected clips: returns clips from region if active, else by individual IDs.
    /// Port of C# EditingService.GetEffectiveSelectedClips (lines 192-206).
    pub fn get_effective_selected_clips<'a>(
        project: &'a Project,
        selected_clip_ids: &[ClipId],
        region: Option<&SelectionRegion>,
    ) -> Vec<&'a TimelineClip> {
        if let Some(region) = region
            && region.is_active {
                let clips_in_region = Self::get_clips_in_region(project, region);
                return clips_in_region.iter().filter_map(|(li, id)| {
                    project.timeline.layers.get(*li).and_then(|l| l.find_clip(id))
                }).collect();
            }

        let mut result = Vec::with_capacity(selected_clip_ids.len());
        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if selected_clip_ids.contains(&clip.id) {
                    result.push(clip);
                }
            }
        }
        result
    }

    /// Select all clips on all layers. Returns clip IDs for the caller to add to selection.
    /// Port of C# EditingService.SelectAllClips (lines 264-276).
    pub fn select_all_clip_ids(project: &Project) -> Vec<(ClipId, i32)> {
        let mut result = Vec::new();
        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                result.push((clip.id.clone(), clip.layer_index));
            }
        }
        result
    }

    /// Compute region bounds from a set of selected clip IDs.
    /// Returns (min_beat, max_beat, min_layer, max_layer) or None if < 2 clips.
    /// Port of C# EditingService.UpdateRegionFromClipSelection (lines 283-303).
    pub fn compute_region_from_clip_selection(
        project: &Project,
        selected_clip_ids: &[ClipId],
    ) -> Option<(f32, f32, i32, i32)> {
        if selected_clip_ids.len() < 2 {
            return None;
        }

        let mut min_beat = f32::MAX;
        let mut max_beat = f32::MIN;
        let mut min_layer = i32::MAX;
        let mut max_layer = i32::MIN;

        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if !selected_clip_ids.contains(&clip.id) {
                    continue;
                }
                if clip.start_beat < min_beat { min_beat = clip.start_beat; }
                if clip.end_beat() > max_beat { max_beat = clip.end_beat(); }
                if clip.layer_index < min_layer { min_layer = clip.layer_index; }
                if clip.layer_index > max_layer { max_layer = clip.layer_index; }
            }
        }

        if min_beat < max_beat {
            Some((min_beat, max_beat, min_layer, max_layer))
        } else {
            None
        }
    }

    // ─── Compound operations (Phase 3B) ───

    /// Cut selected clips: copy to clipboard + delete.
    /// Port of C# EditingService.CutSelectedClips (lines 481-544).
    pub fn cut_clips(
        &mut self,
        project: &mut Project,
        clip_ids: &[ClipId],
        region: Option<&SelectionRegion>,
        spb: f32,
    ) -> Vec<Box<dyn Command>> {
        // Copy to clipboard first
        self.copy_clips(project, clip_ids, region, spb);

        // Delete the clips
        Self::delete_clips(project, clip_ids, region, spb)
    }

    /// Split clips at region boundaries and return the split commands + interior clip IDs.
    /// Port of C# EditingService.SplitClipsForRegionMove (lines 1135-1143).
    #[allow(clippy::type_complexity)]
    pub fn split_clips_for_region_move(
        project: &Project,
        region: &SelectionRegion,
        spb: f32,
    ) -> (Vec<Box<dyn Command>>, Vec<(usize, ClipId)>) {
        let split_cmds = Self::split_clips_at_region_boundaries(project, region, spb);
        let interior = Self::get_clips_in_region(project, region);
        (split_cmds, interior)
    }

    /// Split selected clips at a given beat (playhead).
    /// Port of C# EditingService.SplitSelectedClipsAtPlayhead (lines 1149-1197).
    /// Returns (split_commands, tail_clip_ids) for the caller to update selection.
    pub fn split_clips_at_beat_batch(
        project: &Project,
        clip_ids: &[ClipId],
        split_beat: f32,
        spb: f32,
    ) -> (Vec<Box<dyn Command>>, Vec<ClipId>) {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();
        let mut tail_clip_ids: Vec<ClipId> = Vec::new();

        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if !clip_ids.contains(&clip.id) {
                    continue;
                }
                if let Some(cmd) = Self::split_clip_at_beat(project, &clip.id, split_beat, spb) {
                    // The tail clip ID is generated by SplitClipCommand internally;
                    // caller can find it by looking for new clips after the split beat.
                    commands.push(cmd);
                }
            }
        }

        // Find tail clip IDs after split (clips starting at split_beat on same layers)
        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if (clip.start_beat - split_beat).abs() < 0.001 {
                    tail_clip_ids.push(clip.id.clone());
                }
            }
        }

        (commands, tail_clip_ids)
    }

    /// Toggle mute on selected clips.
    /// Port of C# EditingService.ToggleMuteSelectedClips (lines 418-449).
    pub fn toggle_mute_clips(
        project: &Project,
        clip_ids: &[ClipId],
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        // Determine target state: mute all if any are unmuted
        let any_unmuted = project.timeline.layers.iter()
            .flat_map(|l| l.clips.iter())
            .filter(|c| clip_ids.contains(&c.id))
            .any(|c| !c.is_muted);
        let new_muted = any_unmuted;

        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if clip_ids.contains(&clip.id) && clip.is_muted != new_muted {
                    commands.push(Box::new(MuteClipCommand::new(
                        clip.id.clone(), clip.is_muted, new_muted,
                    )));
                }
            }
        }

        commands
    }

    /// Get the current grid step in beats.
    /// Port of C# EditingService.GetCurrentGridStep (lines 949-954).
    pub fn get_current_grid_step(host: &dyn EditingHost) -> f32 {
        host.grid_interval_beats()
    }

    // ─── Duplicate region restoration (Phase 3E) ───

    /// Duplicate clips with region restoration (Ableton-style).
    /// Returns (commands, new_region) where new_region shifts forward by region duration.
    /// Port of C# EditingService.DuplicateSelectedClips region restoration (lines 743-758).
    pub fn duplicate_clips_with_region(
        project: &Project,
        clip_ids: &[ClipId],
        region: &SelectionRegion,
        spb: f32,
    ) -> (Vec<Box<dyn Command>>, Option<SelectionRegion>) {
        let commands = Self::duplicate_clips(project, clip_ids, region, spb);

        let new_region = if region.is_active && !commands.is_empty() {
            let duration = region.duration_beats();
            Some(SelectionRegion {
                start_beat: region.end_beat,
                end_beat: region.end_beat + duration,
                start_layer_index: region.start_layer_index,
                end_layer_index: region.end_layer_index,
                is_active: true,
                start_layer_id: region.start_layer_id.clone(),
                end_layer_id: region.end_layer_id.clone(),
                selected_layer_ids: region.selected_layer_ids.clone(),
            })
        } else {
            None
        };

        (commands, new_region)
    }
}

impl Default for EditingService {
    fn default() -> Self {
        Self::new()
    }
}
