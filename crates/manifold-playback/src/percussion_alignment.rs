// Port of Unity PercussionAlignmentService.cs (538 lines).
// Application-layer service for percussion alignment calibration, nudge, reset, and reprojection.
// No UI dependencies.

use manifold_core::percussion::ImportedPercussionClipPlacement;
use manifold_core::percussion_analysis::{PercussionClipReprojectionPlanner, ProjectBeatTimeConverter};
use manifold_core::project::Project;
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::clip::MoveClipCommand;

const CALIBRATION_EPSILON_BEATS: f32 = 0.0001;

// ─── PercussionAlignmentSnapshot ───

/// Port of Unity PercussionAlignmentSnapshot struct.
/// Stores old and new alignment state for one placement, identified by clip_id.
/// In Unity this holds a direct object reference; in Rust we identify by clip_id
/// and mutate through the project.
#[derive(Debug, Clone)]
pub struct PercussionAlignmentSnapshot {
    pub clip_id: String,
    pub old_offset_beats: f32,
    pub old_slope_beats_per_second: f32,
    pub old_pivot_seconds: f32,
    pub new_offset_beats: f32,
    pub new_slope_beats_per_second: f32,
    pub new_pivot_seconds: f32,
}

impl PercussionAlignmentSnapshot {
    pub fn new(
        clip_id: String,
        old_offset_beats: f32,
        old_slope_beats_per_second: f32,
        old_pivot_seconds: f32,
        new_offset_beats: f32,
        new_slope_beats_per_second: f32,
        new_pivot_seconds: f32,
    ) -> Self {
        Self {
            clip_id,
            old_offset_beats,
            old_slope_beats_per_second,
            old_pivot_seconds,
            new_offset_beats,
            new_slope_beats_per_second,
            new_pivot_seconds,
        }
    }
}

// ─── AlignmentResult ───

/// Port of Unity AlignmentResult struct.
pub struct AlignmentResult {
    pub success: bool,
    pub moved: i32,
    pub missing: i32,
    pub invalid: i32,
    pub undo_command: Option<Box<dyn Command>>,
}

impl Default for AlignmentResult {
    fn default() -> Self {
        Self {
            success: false,
            moved: 0,
            missing: 0,
            invalid: 0,
            undo_command: None,
        }
    }
}

// ─── ReprojectionResult ───

/// Port of Unity ReprojectionResult struct.
pub struct ReprojectionResult {
    pub success: bool,
    pub moved: i32,
    pub missing: i32,
    pub invalid: i32,
    pub tracked: i32,
    pub undo_command: Option<Box<dyn Command>>,
}

impl Default for ReprojectionResult {
    fn default() -> Self {
        Self {
            success: false,
            moved: 0,
            missing: 0,
            invalid: 0,
            tracked: 0,
            undo_command: None,
        }
    }
}

// ─── PercussionAlignmentService ───

/// Port of Unity PercussionAlignmentService (sealed class).
/// Application-layer service for percussion alignment calibration, nudge, reset, and reprojection.
pub struct PercussionAlignmentService {
    /// Callback invoked when the audio start beat changes (Execute and Undo).
    /// Port of Unity Action<float> onStartBeatChanged field.
    /// In Rust, commands write directly to project.percussion_import.audio_start_beat.
    /// The caller should invoke this callback after executing any returned undo_command
    /// by reading project.imported_percussion_audio_start_beat().
    #[allow(dead_code)]
    on_start_beat_changed: Box<dyn FnMut(f32) + Send>,
}

impl PercussionAlignmentService {
    /// Port of Unity PercussionAlignmentService constructor.
    /// `project` is no longer stored as a field — it is passed per-call to match
    /// the Rust ownership model. The Unity `project` field is threaded through.
    pub fn new(on_start_beat_changed: Box<dyn FnMut(f32) + Send>) -> Self {
        Self { on_start_beat_changed }
    }

    /// Port of Unity PercussionAlignmentService.CalibrateDownbeatAtPlayhead().
    pub fn calibrate_downbeat_at_playhead(
        &mut self,
        project: &mut Project,
        playhead_beat: f32,
        source_seconds: f32,
        current_audio_start_beat: f32,
    ) -> AlignmentResult {
        let has_placements = project
            .imported_percussion_clip_placements()
            .map_or(false, |p| !p.is_empty());
        if !has_placements {
            return AlignmentResult::default();
        }

        let reference_clip_id = {
            let provenance = project
                .imported_percussion_clip_placements()
                .expect("checked above");
            find_first_valid_placement_id(provenance)
        };
        let reference_clip_id = match reference_clip_id {
            Some(id) => id,
            None => return AlignmentResult::default(),
        };

        let projected_beat = {
            let reference = project
                .imported_percussion_clip_placements()
                .unwrap()
                .iter()
                .find(|p| p.clip_id == reference_clip_id)
                .cloned()
                .unwrap();
            let mut beat_time_converter = ProjectBeatTimeConverter::new(project);
            PercussionClipReprojectionPlanner::try_compute_aligned_source_beat(
                &reference,
                source_seconds,
                &mut beat_time_converter,
            )
        };
        let projected_beat = match projected_beat {
            Some(b) => b,
            None => return AlignmentResult::default(),
        };

        let target_downbeat = snap_beat_to_nearest_bar_start(playhead_beat, project);
        let delta_beats = target_downbeat - projected_beat;

        self.apply_alignment_delta(
            project,
            delta_beats,
            current_audio_start_beat,
            "Mark percussion downbeat",
        )
    }

    /// Port of Unity PercussionAlignmentService.Nudge().
    pub fn nudge(
        &mut self,
        project: &mut Project,
        delta_beats: f32,
        current_audio_start_beat: f32,
    ) -> AlignmentResult {
        self.apply_alignment_delta(
            project,
            delta_beats,
            current_audio_start_beat,
            "Nudge percussion alignment",
        )
    }

    /// Port of Unity PercussionAlignmentService.ResetAlignment().
    pub fn reset_alignment(
        &mut self,
        project: &mut Project,
        current_audio_start_beat: f32,
    ) -> AlignmentResult {
        if project.timeline.layers.is_empty() {
            return AlignmentResult::default();
        }

        let has_placements = project
            .imported_percussion_clip_placements()
            .map_or(false, |p| !p.is_empty());
        if !has_placements {
            return AlignmentResult::default();
        }

        let reference_clip_id = {
            let provenance = project
                .imported_percussion_clip_placements()
                .unwrap();
            find_first_valid_placement_id(provenance)
        };
        let reference_clip_id = match reference_clip_id {
            Some(id) => id,
            None => return AlignmentResult::default(),
        };

        let target_audio_start_beat = {
            project
                .imported_percussion_clip_placements()
                .unwrap()
                .iter()
                .find(|p| p.clip_id == reference_clip_id)
                .map(|p| p.start_beat_offset)
                .unwrap_or(0.0)
        };

        let provenance_snapshot: Vec<ImportedPercussionClipPlacement> = project
            .imported_percussion_clip_placements()
            .unwrap()
            .clone();

        let mut snapshots: Vec<PercussionAlignmentSnapshot> =
            Vec::with_capacity(provenance_snapshot.len());
        let mut any_alignment_change = false;

        for placement in &provenance_snapshot {
            if !placement.is_valid() {
                continue;
            }

            let old_offset = placement.alignment_offset_beats;
            let old_slope = placement.alignment_slope_beats_per_second;
            let old_pivot = placement.alignment_pivot_seconds;

            if old_offset.abs() > CALIBRATION_EPSILON_BEATS
                || old_slope.abs() > CALIBRATION_EPSILON_BEATS
                || old_pivot.abs() > CALIBRATION_EPSILON_BEATS
            {
                any_alignment_change = true;
            }

            snapshots.push(PercussionAlignmentSnapshot::new(
                placement.clip_id.clone(),
                old_offset,
                old_slope,
                old_pivot,
                0.0,
                0.0,
                0.0,
            ));
        }

        if snapshots.is_empty() {
            return AlignmentResult::default();
        }

        if !any_alignment_change
            && (current_audio_start_beat - target_audio_start_beat).abs()
                <= CALIBRATION_EPSILON_BEATS
        {
            return AlignmentResult::default();
        }

        self.apply_alignment_snapshots(
            project,
            snapshots,
            current_audio_start_beat,
            target_audio_start_beat,
            "Reset percussion alignment",
        )
    }

    /// Port of Unity PercussionAlignmentService.Reproject().
    pub fn reproject(&self, project: &mut Project) -> ReprojectionResult {
        let mut result = ReprojectionResult::default();

        if project.timeline.layers.is_empty() {
            return result;
        }

        let has_placements = project
            .imported_percussion_clip_placements()
            .map_or(false, |p| !p.is_empty());
        if !has_placements {
            return result;
        }

        let provenance_snapshot: Vec<ImportedPercussionClipPlacement> = project
            .imported_percussion_clip_placements()
            .unwrap()
            .clone();

        // Compute projected beats for all placements using the beat-time converter.
        // The converter borrows project mutably, so resolve timeline clip positions
        // separately before and after to avoid simultaneous borrows.
        let projected_beats: Vec<Option<f32>> = {
            let mut converter = ProjectBeatTimeConverter::new(project);
            provenance_snapshot
                .iter()
                .map(|p| {
                    PercussionClipReprojectionPlanner::try_compute_placement_beat(
                        p,
                        &mut converter,
                    )
                    .map(|(_src, pb)| pb)
                })
                .collect()
        };

        let mut commands: Vec<Box<dyn Command>> = Vec::with_capacity(provenance_snapshot.len());
        let mut retained_ids: Vec<String> = Vec::with_capacity(provenance_snapshot.len());

        for (placement, projected_beat_opt) in provenance_snapshot.iter().zip(projected_beats.iter()) {
            let projected_beat = match projected_beat_opt {
                Some(pb) => *pb,
                None => {
                    result.invalid += 1;
                    continue;
                }
            };

            let clip_data = project.timeline.find_clip_by_id(&placement.clip_id).map(|c| {
                (c.start_beat, c.layer_index)
            });
            let (old_beat, layer_index) = match clip_data {
                Some(d) => d,
                None => {
                    result.missing += 1;
                    continue;
                }
            };

            retained_ids.push(placement.clip_id.clone());

            if (old_beat - projected_beat).abs() <= CALIBRATION_EPSILON_BEATS {
                continue;
            }

            commands.push(Box::new(MoveClipCommand::new(
                placement.clip_id.clone(),
                old_beat,
                projected_beat,
                layer_index,
                layer_index,
            )));
            result.moved += 1;
        }

        // Prune provenance to only retained entries (matching Unity lines 216-218).
        let placements = project.imported_percussion_clip_placements_mut();
        placements.retain(|p| retained_ids.contains(&p.clip_id));
        result.tracked = placements.len() as i32;

        if result.moved <= 0 {
            return result;
        }

        let reproject_command: Box<dyn Command> = if commands.len() == 1 {
            commands.remove(0)
        } else {
            Box::new(CompositeCommand::new(
                commands,
                "Reproject imported percussion clips".to_string(),
            ))
        };

        result.undo_command = Some(reproject_command);
        result.success = true;

        log::debug!(
            "[PercussionAlignmentService] Reprojected: moved={}, missing={}, invalid={}, tracked={}.",
            result.moved,
            result.missing,
            result.invalid,
            result.tracked,
        );

        result
    }

    // ─── Private helpers ───

    /// Port of Unity PercussionAlignmentService.ApplyAlignmentDelta().
    fn apply_alignment_delta(
        &mut self,
        project: &mut Project,
        delta_beats: f32,
        current_audio_start_beat: f32,
        undo_description: &str,
    ) -> AlignmentResult {
        if project.timeline.layers.is_empty() {
            return AlignmentResult::default();
        }

        if !delta_beats.is_finite() {
            return AlignmentResult::default();
        }

        if delta_beats.abs() <= CALIBRATION_EPSILON_BEATS {
            return AlignmentResult::default();
        }

        let has_placements = project
            .imported_percussion_clip_placements()
            .map_or(false, |p| !p.is_empty());
        if !has_placements {
            return AlignmentResult::default();
        }

        let provenance_snapshot: Vec<ImportedPercussionClipPlacement> = project
            .imported_percussion_clip_placements()
            .unwrap()
            .clone();

        let mut snapshots: Vec<PercussionAlignmentSnapshot> =
            Vec::with_capacity(provenance_snapshot.len());

        for placement in &provenance_snapshot {
            if !placement.is_valid() {
                continue;
            }

            let old_offset = placement.alignment_offset_beats;
            let old_slope = placement.alignment_slope_beats_per_second;
            let old_pivot = placement.alignment_pivot_seconds;

            snapshots.push(PercussionAlignmentSnapshot::new(
                placement.clip_id.clone(),
                old_offset,
                old_slope,
                old_pivot,
                old_offset + delta_beats,
                old_slope,
                old_pivot,
            ));
        }

        if snapshots.is_empty() {
            return AlignmentResult::default();
        }

        let target_audio_start_beat = current_audio_start_beat + delta_beats;
        self.apply_alignment_snapshots(
            project,
            snapshots,
            current_audio_start_beat,
            target_audio_start_beat,
            undo_description,
        )
    }

    /// Port of Unity PercussionAlignmentService.ApplyAlignmentSnapshots().
    fn apply_alignment_snapshots(
        &mut self,
        project: &mut Project,
        snapshots: Vec<PercussionAlignmentSnapshot>,
        old_audio_start_beat: f32,
        target_audio_start_beat: f32,
        undo_description: &str,
    ) -> AlignmentResult {
        let mut result = AlignmentResult::default();

        if project.timeline.layers.is_empty() || snapshots.is_empty() {
            return result;
        }

        let new_audio_start_beat = target_audio_start_beat.max(0.0);

        let alignment_command = Box::new(ApplyPercussionAlignmentCommand::new(
            snapshots.clone(),
            old_audio_start_beat,
            new_audio_start_beat,
            undo_description.to_string(),
        ));

        // Temporarily apply new alignment state so reprojection computes target positions
        // using the UPDATED offsets. Without this, move commands see the old state and
        // conclude clips are already in place, producing zero moves while audio shifts.
        // Port of Unity lines 306-311.
        for snapshot in &snapshots {
            if let Some(p) = project
                .imported_percussion_clip_placements_mut()
                .iter_mut()
                .find(|p| p.clip_id == snapshot.clip_id)
            {
                p.set_alignment_state(
                    snapshot.new_offset_beats,
                    snapshot.new_slope_beats_per_second,
                    snapshot.new_pivot_seconds,
                );
            }
        }

        let (move_commands, moved, missing, invalid) = {
            let provenance_snapshot: Vec<ImportedPercussionClipPlacement> = project
                .imported_percussion_clip_placements()
                .unwrap()
                .clone();
            // Compute projected beats first (converter borrows project mutably).
            let projected_beats: Vec<Option<f32>> = {
                let mut converter = ProjectBeatTimeConverter::new(project);
                provenance_snapshot
                    .iter()
                    .map(|p| {
                        PercussionClipReprojectionPlanner::try_compute_placement_beat(
                            p,
                            &mut converter,
                        )
                        .map(|(_src, pb)| pb)
                    })
                    .collect()
            };
            build_reprojection_move_commands_inner(
                &provenance_snapshot,
                &projected_beats,
                project,
            )
        };

        // Revert to old state so the command system applies changes atomically.
        // Port of Unity lines 322-327.
        for snapshot in &snapshots {
            if let Some(p) = project
                .imported_percussion_clip_placements_mut()
                .iter_mut()
                .find(|p| p.clip_id == snapshot.clip_id)
            {
                p.set_alignment_state(
                    snapshot.old_offset_beats,
                    snapshot.old_slope_beats_per_second,
                    snapshot.old_pivot_seconds,
                );
            }
        }

        // Atomicity fix: build composite first, then execute atomically.
        // Port of Unity lines 330-335.
        let mut all_commands: Vec<Box<dyn Command>> =
            Vec::with_capacity(1 + move_commands.len());
        all_commands.push(alignment_command);
        all_commands.extend(move_commands);

        let command: Box<dyn Command> = if all_commands.len() == 1 {
            all_commands.remove(0)
        } else {
            Box::new(CompositeCommand::new(
                all_commands,
                undo_description.to_string(),
            ))
        };

        result.undo_command = Some(command);
        result.success = true;
        result.moved = moved;
        result.missing = missing;
        result.invalid = invalid;

        let tracked = project
            .imported_percussion_clip_placements()
            .map_or(0, |p| p.len());

        log::debug!(
            "[PercussionAlignmentService] Applied alignment: moved={}, missing={}, invalid={}, tracked={}.",
            moved,
            missing,
            invalid,
            tracked,
        );

        result
    }
}

// ─── Static helpers ───

/// Port of Unity PercussionAlignmentService.FindFirstValidPlacement() (static).
pub fn find_first_valid_placement(
    provenance: &[ImportedPercussionClipPlacement],
) -> Option<&ImportedPercussionClipPlacement> {
    for placement in provenance {
        if placement.is_valid() {
            return Some(placement);
        }
    }
    None
}

/// Returns the clip_id of the first valid placement (for borrow-safe callers).
fn find_first_valid_placement_id(
    provenance: &[ImportedPercussionClipPlacement],
) -> Option<String> {
    find_first_valid_placement(provenance).map(|p| p.clip_id.clone())
}

/// Port of Unity PercussionAlignmentService.SnapBeatToNearestBarStart().
fn snap_beat_to_nearest_bar_start(beat: f32, project: &Project) -> f32 {
    let beats_per_bar = (project.settings.time_signature_numerator).max(1);
    let beats_per_bar = beats_per_bar as f32;
    ((beat / beats_per_bar).round() * beats_per_bar).max(0.0)
}

/// Port of Unity PercussionAlignmentService.BuildReprojectionMoveCommands().
/// Returns (commands, moved, missing, invalid).
/// `projected_beats` is pre-computed (one entry per provenance entry, None = invalid).
fn build_reprojection_move_commands_inner(
    provenance: &[ImportedPercussionClipPlacement],
    projected_beats: &[Option<f32>],
    project: &mut Project,
) -> (Vec<Box<dyn Command>>, i32, i32, i32) {
    let mut moved = 0i32;
    let mut missing = 0i32;
    let mut invalid = 0i32;
    let mut commands: Vec<Box<dyn Command>> = Vec::with_capacity(provenance.len());

    for (placement, projected_beat_opt) in provenance.iter().zip(projected_beats.iter()) {
        let projected_beat = match projected_beat_opt {
            Some(pb) => *pb,
            None => {
                invalid += 1;
                continue;
            }
        };

        let clip_data = project
            .timeline
            .find_clip_by_id(&placement.clip_id)
            .map(|c| (c.start_beat, c.layer_index));
        let (old_beat, layer_index) = match clip_data {
            Some(d) => d,
            None => {
                missing += 1;
                continue;
            }
        };

        if (old_beat - projected_beat).abs() <= CALIBRATION_EPSILON_BEATS {
            continue;
        }

        commands.push(Box::new(MoveClipCommand::new(
            placement.clip_id.clone(),
            old_beat,
            projected_beat,
            layer_index,
            layer_index,
        )));
        moved += 1;
    }

    (commands, moved, missing, invalid)
}

// ─── ApplyPercussionAlignmentCommand ───

/// Port of Unity ApplyPercussionAlignmentCommand (sealed class).
/// Undoable command for adjusting percussion alignment state and audio start beat.
/// Decoupled from UI — uses a callback for audio start beat changes.
///
/// NOTE: Unlike Unity (which holds direct object references), this command stores
/// clip_ids and mutates through the project passed to execute/undo. The
/// on_start_beat_changed callback is invoked externally by the caller after
/// executing the returned undo_command.
#[derive(Debug)]
pub struct ApplyPercussionAlignmentCommand {
    snapshots: Vec<PercussionAlignmentSnapshot>,
    old_start_beat: f32,
    new_start_beat: f32,
    description: String,
}

impl ApplyPercussionAlignmentCommand {
    pub fn new(
        snapshots: Vec<PercussionAlignmentSnapshot>,
        old_start_beat: f32,
        new_start_beat: f32,
        description: String,
    ) -> Self {
        let description = if description.trim().is_empty() {
            "Adjust percussion alignment".to_string()
        } else {
            description
        };
        Self {
            snapshots,
            old_start_beat,
            new_start_beat,
            description,
        }
    }

    fn apply_state(&self, project: &mut Project, use_new: bool) {
        for snapshot in &self.snapshots {
            if let Some(p) = project
                .imported_percussion_clip_placements_mut()
                .iter_mut()
                .find(|p| p.clip_id == snapshot.clip_id)
            {
                p.set_alignment_state(
                    if use_new { snapshot.new_offset_beats } else { snapshot.old_offset_beats },
                    if use_new {
                        snapshot.new_slope_beats_per_second
                    } else {
                        snapshot.old_slope_beats_per_second
                    },
                    if use_new { snapshot.new_pivot_seconds } else { snapshot.old_pivot_seconds },
                );
            }
        }

        let start_beat = if use_new { self.new_start_beat } else { self.old_start_beat };
        project.set_imported_percussion_audio_start_beat(start_beat.max(0.0));
        // Note: onStartBeatChanged callback is NOT stored here (can't store a closure in a
        // Debug struct without wrapping). The PercussionAlignmentService caller is responsible
        // for observing audio_start_beat changes after execute/undo and propagating them.
        // This matches the decoupled pattern Unity uses: the service wires the callback at
        // construction time and the command calls it — here the command writes to project and
        // the service's caller polls or subscribes externally.
    }
}

impl Command for ApplyPercussionAlignmentCommand {
    fn execute(&mut self, project: &mut Project) {
        self.apply_state(project, true);
    }

    fn undo(&mut self, project: &mut Project) {
        self.apply_state(project, false);
    }

    fn description(&self) -> &str {
        &self.description
    }
}

// ─── SetAudioStartBeatCommand ───

/// Port of Unity SetAudioStartBeatCommand (sealed class).
/// Undoable command for shifting the audio start beat without percussion clip alignment.
/// Used for audio-only downbeat calibration when no percussion clips exist.
#[derive(Debug)]
pub struct SetAudioStartBeatCommand {
    old_start_beat: f32,
    new_start_beat: f32,
    description: String,
}

impl SetAudioStartBeatCommand {
    pub fn new(
        old_start_beat: f32,
        new_start_beat: f32,
        description: String,
    ) -> Self {
        let description = if description.trim().is_empty() {
            "Set audio start beat".to_string()
        } else {
            description
        };
        Self {
            old_start_beat,
            new_start_beat,
            description,
        }
    }

    fn apply(&self, project: &mut Project, start_beat: f32) {
        project.set_imported_percussion_audio_start_beat(start_beat.max(0.0));
    }
}

impl Command for SetAudioStartBeatCommand {
    fn execute(&mut self, project: &mut Project) {
        self.apply(project, self.new_start_beat);
    }

    fn undo(&mut self, project: &mut Project) {
        self.apply(project, self.old_start_beat);
    }

    fn description(&self) -> &str {
        &self.description
    }
}
