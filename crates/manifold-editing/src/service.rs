use crate::command::{Command, CompositeCommand};
use crate::commands::clip::*;
use crate::commands::layer::DuplicateLayersCommand;
use crate::undo::UndoRedoManager;
use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::selection::SelectionRegion;
use manifold_core::types::LayerType;
use manifold_core::{Beats, ClipId, LayerId, Seconds};
use std::collections::{HashMap, HashSet};

/// Host trait for EditingService — replaces C#'s UIState/CoordinateMapper/PlaybackController.
pub trait EditingHost {
    fn current_beat(&self) -> Beats;
    fn seconds_per_beat(&self) -> f32;
    fn grid_interval_beats(&self) -> Beats;
    fn floor_beat_to_grid(&self, beat: Beats) -> Beats;
    fn snap_beat_to_grid(&self, beat: Beats) -> Beats;
    fn request_clip_sync(&mut self);
    fn mark_compositor_dirty(&mut self);
}

/// Clipboard entry for copy/paste.
#[derive(Debug, Clone)]
struct ClipboardEntry {
    source_clip: TimelineClip,
    beat_offset: Beats,
    layer_offset: i32,
    is_generator: bool,
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

        #[cfg(debug_assertions)]
        Self::warn_overlapping_clips(&project.timeline.layers);
        #[cfg(debug_assertions)]
        project.timeline.debug_assert_tree_order();
    }

    /// Debug-only check: log a warning if any layer has overlapping clips.
    /// Non-fatal — projects saved before the fix may contain pre-existing overlaps.
    #[cfg(debug_assertions)]
    fn warn_overlapping_clips(layers: &[manifold_core::layer::Layer]) {
        for layer in layers {
            if layer.has_overlapping_clips() {
                eprintln!(
                    "[overlap] WARNING: layer {:?} has overlapping clips ({} clips)",
                    layer.layer_id,
                    layer.clips.len(),
                );
            }
        }
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
            #[cfg(debug_assertions)]
            project.timeline.debug_assert_tree_order();
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
            #[cfg(debug_assertions)]
            project.timeline.debug_assert_tree_order();
            true
        } else {
            false
        }
    }

    /// Description of the command `undo()` would act on next. Read this
    /// BEFORE calling `undo()` — see `UndoRedoManager::peek_undo_description`.
    /// D11 undo/redo toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2).
    pub fn peek_undo_description(&self) -> Option<&str> {
        self.undo_manager.peek_undo_description()
    }

    /// Description of the command `redo()` would act on next. Same
    /// peek-before-mutating contract as [`Self::peek_undo_description`].
    pub fn peek_redo_description(&self) -> Option<&str> {
        self.undo_manager.peek_redo_description()
    }

    pub fn can_undo(&self) -> bool {
        self.undo_manager.can_undo()
    }
    pub fn can_redo(&self) -> bool {
        self.undo_manager.can_redo()
    }

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

    pub fn data_version(&self) -> u64 {
        self.data_version
    }

    /// Bump data_version for external mutations that bypass the undo system
    /// (e.g. Ableton bridge mapping changes). This notifies the UI that the
    /// project structure changed without creating an undo entry.
    pub fn notify_external_change(&mut self) {
        self.data_version += 1;
    }

    // ─── Clip lookup ───

    pub fn find_clip_by_id<'a>(
        &self,
        project: &'a mut Project,
        clip_id: &str,
    ) -> Option<&'a TimelineClip> {
        project.timeline.find_clip_by_id(clip_id)
    }

    // ─── Selection helpers ───

    /// Get clips in a selection region.
    pub fn get_clips_in_region(
        project: &Project,
        region: &SelectionRegion,
    ) -> Vec<(usize, ClipId)> {
        if !region.is_active {
            return Vec::new();
        }
        let (min_layer, max_layer) = region
            .layer_index_range(&project.timeline.layers)
            .unwrap_or((0, 0));
        let mut results = Vec::new();

        let region_start = region.start_beat;
        let region_end = region.end_beat;

        for (li, layer) in project.timeline.layers.iter().enumerate() {
            if li < min_layer || li > max_layer {
                continue;
            }
            for clip in &layer.clips {
                if clip.start_beat < region_end && clip.end_beat() > region_start {
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
                commands.push(Box::new(DeleteClipCommand::new(
                    clip.clone(),
                    layer.layer_id.clone(),
                )));
                continue;
            }

            // Case 2: placed clip covers the start -> trim start of existing
            if placed_start <= clip_start && placed_end < clip_end {
                let trim_beats = placed_end - clip_start;
                let trim_seconds = Seconds(trim_beats.0 * spb as f64);
                let new_in_point = clip.in_point + trim_seconds;
                let new_start = placed_end;
                let new_duration = clip.duration_beats - trim_beats;
                commands.push(Box::new(TrimClipCommand::new(
                    clip.id.clone(),
                    clip.start_beat,
                    new_start,
                    clip.duration_beats,
                    new_duration,
                    clip.in_point,
                    new_in_point,
                )));
                continue;
            }

            // Case 3: placed clip covers the end -> trim end of existing
            if placed_start > clip_start && placed_end >= clip_end {
                let new_duration = placed_start - clip_start;
                commands.push(Box::new(TrimClipCommand::new(
                    clip.id.clone(),
                    clip.start_beat,
                    clip.start_beat,
                    clip.duration_beats,
                    new_duration,
                    clip.in_point,
                    clip.in_point,
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
                tail.in_point = clip.in_point + Seconds(beats_elapsed.0 * spb as f64);

                commands.push(Box::new(TrimClipCommand::new(
                    clip.id.clone(),
                    clip.start_beat,
                    clip.start_beat,
                    clip.duration_beats,
                    new_duration,
                    clip.in_point,
                    clip.in_point,
                )));
                commands.push(Box::new(AddClipCommand::new_with_ignore_ids(
                    tail,
                    layer.layer_id.clone(),
                    spb,
                    ignore_ids.clone(),
                )));
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
            && region.is_active
        {
            let region_start = region.start_beat;
            let region_end = region.end_beat;

            // Find overlapping clips in region (matching Unity CopySelectedClips)
            let overlapping: Vec<(&TimelineClip, usize)> = project
                .timeline
                .layers
                .iter()
                .enumerate()
                .flat_map(|(li, l)| l.clips.iter().map(move |c| (c, li)))
                .filter(|(c, _)| {
                    c.start_beat < region_end
                        && c.end_beat() > region_start
                        && clip_ids.contains(&c.id)
                })
                .collect();

            if overlapping.is_empty() {
                return;
            }

            let origin_beat = region_start;
            let (min_layer, _) = region
                .layer_index_range(&project.timeline.layers)
                .unwrap_or((0, 0));

            for (clip, clip_layer_idx) in overlapping {
                let trimmed = Self::trim_clip_to_region(clip, region, spb);
                let is_gen = project
                    .timeline
                    .layers
                    .get(clip_layer_idx)
                    .is_some_and(|l| l.layer_type == LayerType::Generator);
                self.clipboard.push(ClipboardEntry {
                    beat_offset: trimmed.start_beat - origin_beat,
                    layer_offset: clip_layer_idx as i32 - min_layer as i32,
                    source_clip: trimmed,
                    is_generator: is_gen,
                });
            }
            return;
        }

        // Individual mode: copy full clips, earliest-clip origin
        if clip_ids.is_empty() {
            return;
        }

        let mut clips_with_layer: Vec<(TimelineClip, usize)> = Vec::new();
        for (li, layer) in project.timeline.layers.iter().enumerate() {
            for clip in &layer.clips {
                if clip_ids.contains(&clip.id) {
                    clips_with_layer.push((clip.clone(), li));
                }
            }
        }

        if clips_with_layer.is_empty() {
            return;
        }

        let min_beat = clips_with_layer
            .iter()
            .map(|(c, _)| c.start_beat)
            .fold(Beats(f64::MAX), Beats::min);
        let min_layer_idx = clips_with_layer
            .iter()
            .map(|(_, li)| *li as i32)
            .min()
            .unwrap_or(0);

        for (clip, li) in clips_with_layer {
            let is_gen = project
                .timeline
                .layers
                .get(li)
                .is_some_and(|l| l.layer_type == LayerType::Generator);
            self.clipboard.push(ClipboardEntry {
                beat_offset: clip.start_beat - min_beat,
                layer_offset: li as i32 - min_layer_idx,
                source_clip: clip,
                is_generator: is_gen,
            });
        }
    }

    /// Paste clips from clipboard at the given position.
    /// Matches Unity PasteClips: skips gen/video type mismatches,
    /// adopts target layer's generator type, enforces non-overlap.
    pub fn paste_clips(
        &self,
        project: &mut Project,
        target_beat: Beats,
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
                None => {
                    skipped += 1;
                    continue;
                }
            };

            let clip_is_gen = entry.is_generator;
            let layer_is_gen = layer.layer_type == LayerType::Generator;

            // Gen<->video mismatch: skip
            if clip_is_gen != layer_is_gen {
                skipped += 1;
                continue;
            }

            let mut new_clip = entry.source_clip.clone_with_new_id();
            new_clip.start_beat = paste_beat;

            let paste_layer_id = layer.layer_id.clone();
            pasted_ids.push(new_clip.id.clone());
            // AddClipCommand enforces non-overlap internally.
            commands.push(Box::new(AddClipCommand::new(new_clip, paste_layer_id, spb)));
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
        split_beat: Beats,
        spb: f32,
    ) -> Option<SplitClipCommand> {
        for layer in &project.timeline.layers {
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
                if !clip.video_clip_id.is_empty() && clip.duration_beats > Beats::ZERO {
                    tail.in_point = clip.in_point + Seconds(new_duration.0 * spb as f64);
                }
                return Some(SplitClipCommand::new(
                    clip.id.clone(),
                    layer.layer_id.clone(),
                    clip.duration_beats,
                    new_duration,
                    tail,
                ));
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
        Self::split_clips_at_region_boundaries_with_interior(project, region, spb).0
    }

    /// Split clips at region boundaries AND return the post-split interior clip IDs.
    ///
    /// The interior IDs are the clip IDs that will represent the region-interior
    /// segments AFTER the split commands have executed:
    /// - Both boundaries split → tail from the start split (new ID)
    /// - Only start split → tail from the start split (new ID)
    /// - Only end split → original clip ID (shortened to interior)
    /// - Fully inside → original clip ID (unchanged)
    #[allow(clippy::type_complexity)]
    pub fn split_clips_at_region_boundaries_with_interior(
        project: &Project,
        region: &SelectionRegion,
        spb: f32,
    ) -> (Vec<Box<dyn Command>>, Vec<(usize, ClipId)>) {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();
        let mut interior_ids: Vec<(usize, ClipId)> = Vec::new();

        let layer_count = project.timeline.layers.len();
        let (start_layer, end_layer) = region
            .layer_index_range(&project.timeline.layers)
            .map(|(lo, hi)| {
                (
                    lo.min(layer_count.saturating_sub(1)),
                    hi.min(layer_count.saturating_sub(1)),
                )
            })
            .unwrap_or((0, 0));

        let region_start = region.start_beat;
        let region_end = region.end_beat;

        for li in start_layer..=end_layer {
            // Snapshot clip IDs (splits add new clips)
            let clip_ids: Vec<ClipId> = project.timeline.layers[li]
                .clips
                .iter()
                .map(|c| c.id.clone())
                .collect();

            for clip_id in &clip_ids {
                let clip = match project.timeline.layers[li].find_clip(clip_id) {
                    Some(c) => c,
                    None => continue,
                };

                if clip.end_beat() <= region_start || clip.start_beat >= region_end {
                    continue;
                }

                let straddles_end = clip.end_beat() > region_end;
                let straddles_start = clip.start_beat < region_start;

                // Split at region end FIRST (so the original's EndBeat is still valid
                // when we split at region start)
                if straddles_end
                    && let Some(cmd) = Self::split_clip_at_beat(project, clip_id, region_end, spb)
                {
                    commands.push(Box::new(cmd));
                }

                // Split at region start
                let start_split_tail_id = if straddles_start
                    && let Some(cmd) = Self::split_clip_at_beat(project, clip_id, region_start, spb)
                {
                    // The tail from this split IS the interior piece
                    let tail_id = cmd.tail_clip_id().clone();
                    commands.push(Box::new(cmd));
                    Some(tail_id)
                } else {
                    None
                };

                // Determine the interior clip ID:
                // - If we split at start, the tail is the interior piece
                // - Otherwise, the original clip ID is the interior piece
                //   (either shortened by end-split, or fully inside)
                let interior_id = start_split_tail_id.unwrap_or_else(|| clip_id.clone());
                interior_ids.push((li, interior_id));
            }
        }

        (commands, interior_ids)
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
        let region_start = region.start_beat;
        let region_end = region.end_beat;

        let new_start = clip.start_beat.max(region_start);
        let new_end = clip.end_beat().min(region_end);
        let new_duration = (new_end - new_start).max(Beats::ZERO);

        let mut trimmed = clip.clone_with_new_id();
        trimmed.start_beat = new_start;
        trimmed.duration_beats = new_duration;

        // Adjust in_point for video clips
        if !clip.video_clip_id.is_empty() {
            trimmed.in_point =
                clip.in_point + Seconds((new_start - clip.start_beat).0 * spb as f64);
        }

        trimmed
    }

    // ─── Create clip ───

    /// Create a new clip at the given beat and layer.
    /// Returns None if the target layer is a group (groups cannot hold clips).
    /// Returns (command, clip_id) so the caller can track the new clip.
    pub fn create_clip_at_position(
        project: &mut Project,
        beat: Beats,
        layer_index: usize,
        duration_beats: Beats,
        spb: f32,
    ) -> Option<(Box<dyn Command>, ClipId)> {
        let layer = project.timeline.layers.get(layer_index)?;
        if layer.is_group() {
            return None;
        }
        let is_generator = layer.layer_type == LayerType::Generator;
        let layer_id = layer.layer_id.clone();

        let clip = if is_generator {
            TimelineClip::new_generator(beat, duration_beats)
        } else {
            TimelineClip {
                start_beat: beat,
                duration_beats,
                ..Default::default()
            }
        };

        let clip_id = clip.id.clone();
        Some((Box::new(AddClipCommand::new(clip, layer_id, spb)), clip_id))
    }

    // ─── Duplicate ───

    /// Duplicate selected clips, shifting them forward.
    /// Region mode: trims clips to region boundaries, places copies after region end.
    /// Individual mode: places copies offset by the selected clips' span.
    /// Enforces non-overlap for each new clip (same pattern as paste_clips).
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
            let region_start = region.start_beat;
            let region_end = region.end_beat;
            let (start_layer, end_layer) = region
                .layer_index_range(&project.timeline.layers)
                .map(|(lo, hi)| (lo, hi.min(project.timeline.layers.len().saturating_sub(1))))
                .unwrap_or((0, 0));

            for li in start_layer..=end_layer {
                let layer = &project.timeline.layers[li];
                for clip in &layer.clips {
                    // Overlap test: clip intersects region (Unity lines 182-183)
                    if clip.end_beat() <= region_start || clip.start_beat >= region_end {
                        continue;
                    }
                    let trimmed = Self::trim_clip_to_region(clip, region, spb);
                    let mut new_clip = trimmed;
                    new_clip.start_beat += offset;

                    // AddClipCommand enforces non-overlap internally.
                    commands.push(Box::new(AddClipCommand::new(
                        new_clip,
                        layer.layer_id.clone(),
                        spb,
                    )));
                }
            }
        } else {
            // Individual mode: offset by the clips' own span
            let mut min_beat = Beats(f64::MAX);
            let mut max_end = Beats(f64::MIN);
            for layer in &project.timeline.layers {
                for clip in &layer.clips {
                    if clip_ids.contains(&clip.id) {
                        min_beat = min_beat.min(clip.start_beat);
                        max_end = max_end.max(clip.end_beat());
                    }
                }
            }
            let shift = if max_end > min_beat {
                max_end - min_beat
            } else {
                Beats::ONE
            };

            for layer in &project.timeline.layers {
                for clip in &layer.clips {
                    if clip_ids.contains(&clip.id) {
                        let mut new_clip = clip.clone_with_new_id();
                        new_clip.start_beat += shift;

                        // AddClipCommand enforces non-overlap internally.
                        commands.push(Box::new(AddClipCommand::new(
                            new_clip,
                            layer.layer_id.clone(),
                            spb,
                        )));
                    }
                }
            }
        }

        commands
    }

    /// Build a command that drops a copy of `src_clip_id` at `target_beat` on
    /// `target_layer` — the opt/alt-drag-duplicate primitive. The clone keeps all
    /// of the source clip's data (generator, effects, in_point, …) with a fresh
    /// id. `AddClipCommand` enforces non-overlap internally. Returns None if the
    /// source clip or target layer is missing.
    pub fn duplicate_clip_to(
        project: &Project,
        src_clip_id: &ClipId,
        target_beat: Beats,
        target_layer: usize,
        spb: f32,
    ) -> Option<Box<dyn Command>> {
        let src = project
            .timeline
            .layers
            .iter()
            .flat_map(|l| l.clips.iter())
            .find(|c| c.id == *src_clip_id)?;
        let target_layer_id = project.timeline.layers.get(target_layer)?.layer_id.clone();
        let mut new_clip = src.clone_with_new_id();
        new_clip.start_beat = target_beat;
        Some(Box::new(AddClipCommand::new(new_clip, target_layer_id, spb)))
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
            && region.is_active
        {
            // Region mode: split at boundaries, then delete interior clips.
            // We must determine interior IDs at split-build time (not from
            // the pre-split project), because splits change which ID maps to
            // which segment.
            let (split_cmds, interior_ids) =
                Self::split_clips_at_region_boundaries_with_interior(project, region, spb);
            commands.extend(split_cmds);

            // Build delete commands for the interior clips.
            // For clips that were split, we need the pre-split clip data to
            // construct DeleteClipCommand (it stores the full clip for undo).
            // The interior ID is either the original (if no start-split) or
            // a new tail (if start-split). For new tails we build a synthetic
            // clip snapshot matching what the tail will look like after splits.
            for (li, interior_id) in &interior_ids {
                let layer = &project.timeline.layers[*li];
                let lid = layer.layer_id.clone();

                if let Some(clip) = layer.find_clip(interior_id) {
                    // Interior is the original clip (only end-split or fully inside):
                    // After splits, its duration may shrink but its ID stays.
                    // The delete command records the PRE-split state for undo;
                    // that's fine because undo reverses splits after re-adding.
                    commands.push(Box::new(DeleteClipCommand::new(clip.clone(), lid)));
                } else {
                    // Interior is a NEW tail from a start-split. It doesn't exist
                    // in the project yet — find the original clip and build the
                    // trimmed interior snapshot that the split will produce.
                    // The original clip ID is the one that was split (the clip on
                    // this layer that overlaps the region).
                    let region_start = region.start_beat;
                    let region_end = region.end_beat;
                    if let Some(orig) = layer
                        .clips
                        .iter()
                        .find(|c| c.start_beat < region_start && c.end_beat() > region_start)
                    {
                        let tail_end = orig.end_beat().min(region_end);
                        let mut interior_clip = orig.clone();
                        interior_clip.id = interior_id.clone();
                        interior_clip.start_beat = region_start;
                        interior_clip.duration_beats = tail_end - region_start;
                        if !orig.video_clip_id.is_empty() && orig.duration_beats > Beats::ZERO {
                            let offset = region_start - orig.start_beat;
                            interior_clip.in_point = orig.in_point + Seconds(offset.0 * spb as f64);
                        }
                        commands.push(Box::new(DeleteClipCommand::new(interior_clip, lid)));
                    }
                }
            }

            return commands;
        }

        // Individual selection path
        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if clip_ids.contains(&clip.id) {
                    commands.push(Box::new(DeleteClipCommand::new(
                        clip.clone(),
                        layer.layer_id.clone(),
                    )));
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
        beat_delta: Beats,
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
                let new_start = (old_start + beat_delta).max(Beats::ZERO);
                if (new_start - old_start).abs() < Beats(0.0001) {
                    continue;
                }

                commands.push(Box::new(MoveClipCommand::new(
                    clip.id.clone(),
                    old_start,
                    new_start,
                    layer.layer_id.clone(),
                    layer.layer_id.clone(),
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
                project,
                &nudged.clip,
                nudged.layer_index,
                &nudged_ids,
                spb,
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
        grid_step: Beats,
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if !clip_ids.contains(&clip.id) {
                    continue;
                }

                let mut new_duration = clip.duration_beats + grid_step;

                // Clamp to next clip on same layer to prevent overlap
                let mut next_start = Beats(f64::MAX);
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
                if (new_duration - clip.duration_beats).abs() < Beats(0.001) {
                    continue;
                }
                if new_duration <= clip.duration_beats {
                    continue;
                }

                commands.push(Box::new(TrimClipCommand::new(
                    clip.id.clone(),
                    clip.start_beat,
                    clip.start_beat,
                    clip.duration_beats,
                    new_duration,
                    clip.in_point,
                    clip.in_point,
                )));
            }
        }

        commands
    }

    /// Shrink selected clips by one grid step.
    pub fn shrink_clips_by_grid(
        project: &Project,
        clip_ids: &[ClipId],
        grid_step: Beats,
    ) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();
        let min_duration = Beats(0.25); // Fixed 1/16th note minimum. Port of Unity line 861: const float minDuration = 0.25f;

        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if clip_ids.contains(&clip.id) {
                    let new_duration = (clip.duration_beats - grid_step).max(min_duration);
                    if (new_duration - clip.duration_beats).abs() > Beats(0.001) {
                        commands.push(Box::new(TrimClipCommand::new(
                            clip.id.clone(),
                            clip.start_beat,
                            clip.start_beat,
                            clip.duration_beats,
                            new_duration,
                            clip.in_point,
                            clip.in_point,
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

        let target_layer_id = target_layer.layer_id.clone();

        for layer in &project.timeline.layers {
            if let Some(clip) = layer.find_clip(clip_id) {
                if layer.layer_id == target_layer_id {
                    return None;
                }

                // Gen/video type mismatch: block
                let clip_is_gen = layer.layer_type == LayerType::Generator;
                let target_is_gen = target_layer.layer_type == LayerType::Generator;
                if clip_is_gen != target_is_gen {
                    return None;
                }

                // MoveClipCommand handles generator type adoption internally
                return Some(Box::new(MoveClipCommand::new(
                    clip.id.clone(),
                    clip.start_beat,
                    clip.start_beat,
                    layer.layer_id.clone(),
                    target_layer_id,
                )));
            }
        }
        None
    }

    /// Move a whole clip selection across layers by a fixed layer-index delta
    /// (keyboard Up/Down, B14 — `docs/TIMELINE_INTERACTION_P1_SPEC.md` §5 P1.6).
    /// All-or-nothing: if ANY selected clip's destination would fall outside
    /// the layer range, land on a group layer, or cross the gen/video type
    /// boundary, the whole press is a no-op — mirrors the drag cross-layer
    /// block in `interaction_overlay.rs`'s `layer_delta` clamp (the nearest
    /// in-repo precedent for moving a multi-clip selection across layers;
    /// same-layer keyboard nudge's per-clip skip in `nudge_clips` does NOT
    /// apply here — a partial cross-layer move would strand some clips on a
    /// layer the user never chose). Reuses `move_clip_to_layer` for command
    /// construction — no new Command type — then `enforce_non_overlap` on
    /// each destination layer, same as `nudge_clips`, so the write-time
    /// non-overlap invariant holds for the target lane too.
    pub fn move_clips_across_layers(
        project: &Project,
        clip_ids: &[ClipId],
        layer_delta: i32,
        spb: f32,
    ) -> Vec<Box<dyn Command>> {
        if layer_delta == 0 || clip_ids.is_empty() {
            return Vec::new();
        }
        let total_layers = project.timeline.layers.len() as i32;

        // Resolve each selected clip's current layer index once, up front —
        // against the pre-move project, same as `nudge_clips`.
        let mut sources: Vec<(TimelineClip, i32)> = Vec::new();
        for (li, layer) in project.timeline.layers.iter().enumerate() {
            for clip in &layer.clips {
                if clip_ids.contains(&clip.id) {
                    sources.push((clip.clone(), li as i32));
                }
            }
        }
        if sources.is_empty() {
            return Vec::new();
        }

        // All-or-nothing gate.
        for (_, li) in &sources {
            let dest = li + layer_delta;
            if dest < 0 || dest >= total_layers {
                return Vec::new();
            }
            let dest_layer = &project.timeline.layers[dest as usize];
            if dest_layer.is_group() {
                return Vec::new();
            }
            let src_is_gen =
                project.timeline.layers[*li as usize].layer_type == LayerType::Generator;
            let dst_is_gen = dest_layer.layer_type == LayerType::Generator;
            if src_is_gen != dst_is_gen {
                return Vec::new();
            }
        }

        let mut commands: Vec<Box<dyn Command>> = Vec::new();
        let moved_ids: HashSet<ClipId> = sources.iter().map(|(c, _)| c.id.clone()).collect();
        for (clip, li) in &sources {
            let dest = li + layer_delta;
            if let Some(cmd) = Self::move_clip_to_layer(project, clip.id.as_str(), dest) {
                commands.push(cmd);
            }
            // Overlap enforcement on the destination layer, excluding the
            // other clips moving in this same batch (they resolve against
            // each other's final positions only if they actually collide —
            // same semantics as `nudge_clips`). The clip's own beat position
            // is unchanged by a cross-layer move, so `clip` itself is already
            // the correct post-move shape.
            let overlap_cmds =
                Self::enforce_non_overlap(project, clip, dest as usize, &moved_ids, spb);
            commands.extend(overlap_cmds);
        }
        commands
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
            && region.is_active
        {
            let clips_in_region = Self::get_clips_in_region(project, region);
            return clips_in_region
                .iter()
                .filter_map(|(li, id)| {
                    project
                        .timeline
                        .layers
                        .get(*li)
                        .and_then(|l| l.find_clip(id))
                })
                .collect();
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
    pub fn select_all_clip_ids(project: &Project) -> Vec<(ClipId, LayerId)> {
        let mut result = Vec::new();
        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                result.push((clip.id.clone(), layer.layer_id.clone()));
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
    ) -> Option<(Beats, Beats, i32, i32)> {
        if selected_clip_ids.len() < 2 {
            return None;
        }

        let mut min_beat = Beats(f64::MAX);
        let mut max_beat = Beats(f64::MIN);
        let mut min_layer = i32::MAX;
        let mut max_layer = i32::MIN;

        for (li, layer) in project.timeline.layers.iter().enumerate() {
            for clip in &layer.clips {
                if !selected_clip_ids.contains(&clip.id) {
                    continue;
                }
                let li = li as i32;
                if clip.start_beat < min_beat {
                    min_beat = clip.start_beat;
                }
                if clip.end_beat() > max_beat {
                    max_beat = clip.end_beat();
                }
                if li < min_layer {
                    min_layer = li;
                }
                if li > max_layer {
                    max_layer = li;
                }
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
        Self::split_clips_at_region_boundaries_with_interior(project, region, spb)
    }

    /// Split selected clips at a given beat (playhead).
    /// Port of C# EditingService.SplitSelectedClipsAtPlayhead (lines 1149-1197).
    /// Returns (split_commands, tail_clip_ids) for the caller to update selection.
    pub fn split_clips_at_beat_batch(
        project: &Project,
        clip_ids: &[ClipId],
        split_beat: Beats,
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
                    tail_clip_ids.push(cmd.tail_clip_id().clone());
                    commands.push(Box::new(cmd));
                }
            }
        }

        (commands, tail_clip_ids)
    }

    /// Toggle mute on selected clips.
    /// Port of C# EditingService.ToggleMuteSelectedClips (lines 418-449).
    pub fn toggle_mute_clips(project: &Project, clip_ids: &[ClipId]) -> Vec<Box<dyn Command>> {
        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        // Determine target state: mute all if any are unmuted
        let any_unmuted = project
            .timeline
            .layers
            .iter()
            .flat_map(|l| l.clips.iter())
            .filter(|c| clip_ids.contains(&c.id))
            .any(|c| !c.is_muted);
        let new_muted = any_unmuted;

        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                if clip_ids.contains(&clip.id) && clip.is_muted != new_muted {
                    commands.push(Box::new(MuteClipCommand::new(
                        clip.id.clone(),
                        clip.is_muted,
                        new_muted,
                    )));
                }
            }
        }

        commands
    }

    /// Get the current grid step in beats.
    /// Port of C# EditingService.GetCurrentGridStep (lines 949-954).
    pub fn get_current_grid_step(host: &dyn EditingHost) -> Beats {
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

    /// Duplicate one or more layers (Ableton-style: deep copy inserted below the last selected).
    /// Groups are expanded to include all descendants; parent_layer_id refs are remapped.
    pub fn duplicate_layers(project: &Project, layer_ids: &[LayerId]) -> Option<Box<dyn Command>> {
        if layer_ids.is_empty() {
            return None;
        }

        // 1. Expand selection to include all descendants (deep-copy groups).
        let mut expanded_ids: HashSet<LayerId> = layer_ids.iter().cloned().collect();
        let mut to_check: Vec<LayerId> = layer_ids.to_vec();
        while let Some(parent_id) = to_check.pop() {
            for layer in &project.timeline.layers {
                if layer.parent_layer_id.as_ref() == Some(&parent_id)
                    && !expanded_ids.contains(&layer.layer_id)
                {
                    expanded_ids.insert(layer.layer_id.clone());
                    to_check.push(layer.layer_id.clone());
                }
            }
        }

        // 2. Collect expanded set in timeline order; find the last index.
        let expanded_in_order: Vec<(usize, &Layer)> = project
            .timeline
            .layers
            .iter()
            .enumerate()
            .filter(|(_, l)| expanded_ids.contains(&l.layer_id))
            .collect();

        if expanded_in_order.is_empty() {
            return None;
        }

        let insert_after_index = expanded_in_order.last().unwrap().0 + 1;

        // 3. Clone each layer with fresh IDs; build old→new LayerId map for parent remapping.
        let mut id_map: HashMap<LayerId, LayerId> = HashMap::new();
        let mut new_layers: Vec<Layer> = expanded_in_order
            .iter()
            .map(|(_, l)| {
                let cloned = l.clone_with_new_ids();
                id_map.insert(l.layer_id.clone(), cloned.layer_id.clone());
                cloned
            })
            .collect();

        // 4. Remap parent_layer_id on cloned layers whose parent was also duplicated.
        for layer in &mut new_layers {
            if let Some(ref old_parent) = layer.parent_layer_id.clone()
                && let Some(new_parent) = id_map.get(old_parent)
            {
                layer.parent_layer_id = Some(new_parent.clone());
            }
        }

        Some(Box::new(DuplicateLayersCommand::new(
            new_layers,
            insert_after_index,
        )))
    }
}

impl Default for EditingService {
    fn default() -> Self {
        Self::new()
    }
}
