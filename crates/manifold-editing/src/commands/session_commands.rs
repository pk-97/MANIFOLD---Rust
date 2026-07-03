//! Grid-editing commands for Session Mode (P3): scene CRUD, slot set/clear,
//! and timeline<->session conversion (capture/paste). See
//! `docs/SESSION_MODE_DESIGN.md` §7. Normal undoable `Command`s — launches
//! (`ContentCommand::SessionLaunch*`, P2) are a separate, non-undoable path.
//!
//! Reuse, not reimplementation, per §7/DESIGN_DOC_STANDARD §6:
//! - Capture's head/tail trim (including the `in_point` advance for video
//!   clips) goes through `EditingService::trim_clip_to_region`
//!   (`crates/manifold-editing/src/service.rs`), the same pure helper the
//!   region-copy/duplicate path already uses. Not reimplemented here.
//! - Fresh clip identities in both directions go through
//!   `TimelineClip::clone_with_new_id` (`crates/manifold-core/src/clip.rs`),
//!   the same "duplicated" path `EditingService::trim_clip_to_region` and
//!   `split_clip_at_beat` already use — so a slot never shares `ClipId`s
//!   with the lane it came from, in either direction.
//! - Paste's collision handling goes through the existing `Layer::add_clip`
//!   (`crates/manifold-core/src/layer.rs`), which calls
//!   `enforce_non_overlap_for` internally — same pattern as `AddClipCommand`
//!   (`crates/manifold-editing/src/commands/clip.rs`), whose undo logic for
//!   reversing `OverlapAction`s is mirrored here for a whole sequence of
//!   clips instead of one.

use crate::command::Command;
use crate::service::EditingService;
use manifold_core::clip::TimelineClip;
use manifold_core::layer::OverlapAction;
use manifold_core::project::Project;
use manifold_core::selection::SelectionRegion;
use manifold_core::session::{ClipSequence, Scene, SessionSlot};
use manifold_core::types::LayerType;
use manifold_core::{Beats, ClipId, LayerId, SceneId};

// ─── Scene CRUD ───

/// Add a new scene (row) to the session grid at `insert_index`.
#[derive(Debug)]
pub struct AddSceneCommand {
    scene: Scene,
    insert_index: usize,
}

impl AddSceneCommand {
    pub fn new(scene: Scene, insert_index: usize) -> Self {
        Self {
            scene,
            insert_index,
        }
    }
}

impl Command for AddSceneCommand {
    fn execute(&mut self, project: &mut Project) {
        let idx = self.insert_index.min(project.session.scenes.len());
        project.session.scenes.insert(idx, self.scene.clone());
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(pos) = project
            .session
            .scenes
            .iter()
            .position(|s| s.id == self.scene.id)
        {
            project.session.scenes.remove(pos);
        }
    }

    fn description(&self) -> &str {
        "Add Scene"
    }
}

/// Remove a scene (row) from the session grid.
///
/// Grid integrity: a scene with no slots referencing it is a data-model
/// invariant the same way a layer with no slots is (§7) — so removal takes
/// that scene's slots out in the same command, restored on undo.
#[derive(Debug)]
pub struct RemoveSceneCommand {
    scene_id: SceneId,
    removed_scene: Option<Scene>,
    removed_at_index: usize,
    removed_slots: Vec<SessionSlot>,
}

impl RemoveSceneCommand {
    pub fn new(scene_id: SceneId) -> Self {
        Self {
            scene_id,
            removed_scene: None,
            removed_at_index: 0,
            removed_slots: Vec::new(),
        }
    }
}

impl Command for RemoveSceneCommand {
    fn execute(&mut self, project: &mut Project) {
        self.removed_scene = None;
        self.removed_slots.clear();

        if let Some(idx) = project
            .session
            .scenes
            .iter()
            .position(|s| s.id == self.scene_id)
        {
            self.removed_at_index = idx;
            self.removed_scene = Some(project.session.scenes.remove(idx));
        }

        let scene_id = self.scene_id.clone();
        let mut i = 0;
        while i < project.session.slots.len() {
            if project.session.slots[i].scene_id == scene_id {
                self.removed_slots.push(project.session.slots.remove(i));
            } else {
                i += 1;
            }
        }
        project.session.mark_slot_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(scene) = self.removed_scene.take() {
            let idx = self.removed_at_index.min(project.session.scenes.len());
            project.session.scenes.insert(idx, scene);
        }
        for slot in self.removed_slots.drain(..) {
            project.session.slots.push(slot);
        }
        project.session.mark_slot_lookup_dirty();
    }

    fn description(&self) -> &str {
        "Remove Scene"
    }
}

/// Rename a scene.
#[derive(Debug)]
pub struct RenameSceneCommand {
    scene_id: SceneId,
    old_name: String,
    new_name: String,
}

impl RenameSceneCommand {
    pub fn new(scene_id: SceneId, old_name: String, new_name: String) -> Self {
        Self {
            scene_id,
            old_name,
            new_name,
        }
    }
}

impl Command for RenameSceneCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(scene) = project
            .session
            .scenes
            .iter_mut()
            .find(|s| s.id == self.scene_id)
        {
            scene.name = self.new_name.clone();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(scene) = project
            .session
            .scenes
            .iter_mut()
            .find(|s| s.id == self.scene_id)
        {
            scene.name = self.old_name.clone();
        }
    }

    fn description(&self) -> &str {
        "Rename Scene"
    }
}

/// Reorder scenes atomically. Same whole-list-swap shape as
/// `ReorderLayerCommand` (`crates/manifold-editing/src/commands/layer.rs`).
#[derive(Debug)]
pub struct ReorderSceneCommand {
    old_order: Vec<Scene>,
    new_order: Vec<Scene>,
}

impl ReorderSceneCommand {
    pub fn new(old_order: Vec<Scene>, new_order: Vec<Scene>) -> Self {
        Self {
            old_order,
            new_order,
        }
    }
}

impl Command for ReorderSceneCommand {
    fn execute(&mut self, project: &mut Project) {
        project.session.scenes = self.new_order.clone();
    }

    fn undo(&mut self, project: &mut Project) {
        project.session.scenes = self.old_order.clone();
    }

    fn description(&self) -> &str {
        "Reorder Scenes"
    }
}

// ─── Slot set/clear ───

/// Set, replace, or clear the slot at (layer, scene). `slot: None` clears
/// the cell back to sparse-empty.
#[derive(Debug)]
pub struct SetSlotCommand {
    layer_id: LayerId,
    scene_id: SceneId,
    new_slot: Option<SessionSlot>,
    prior_slot: Option<SessionSlot>,
}

impl SetSlotCommand {
    pub fn new(layer_id: LayerId, scene_id: SceneId, slot: Option<SessionSlot>) -> Self {
        Self {
            layer_id,
            scene_id,
            new_slot: slot,
            prior_slot: None,
        }
    }
}

impl Command for SetSlotCommand {
    fn execute(&mut self, project: &mut Project) {
        let pos = project
            .session
            .slots
            .iter()
            .position(|s| s.layer_id == self.layer_id && s.scene_id == self.scene_id);
        self.prior_slot = pos.map(|i| project.session.slots.remove(i));

        if let Some(slot) = self.new_slot.clone() {
            project.session.slots.push(slot);
        }
        project.session.mark_slot_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        if self.new_slot.is_some()
            && let Some(i) = project
                .session
                .slots
                .iter()
                .position(|s| s.layer_id == self.layer_id && s.scene_id == self.scene_id)
        {
            project.session.slots.remove(i);
        }
        if let Some(prior) = self.prior_slot.take() {
            project.session.slots.push(prior);
        }
        project.session.mark_slot_lookup_dirty();
    }

    fn description(&self) -> &str {
        "Set Session Slot"
    }
}

// ─── Timeline <-> session conversion ───

/// Find the nearest marker at or before `beat`, returning its name if it has
/// one. `None` (no marker, or an unnamed one) means the caller falls back to
/// "Scene N" — see `docs/SESSION_MODE_DESIGN.md` §7.
fn nearest_marker_name_at_or_before(project: &Project, beat: Beats) -> Option<String> {
    project
        .timeline
        .markers
        .iter()
        .filter(|m| m.beat <= beat)
        .max_by(|a, b| {
            a.beat
                .partial_cmp(&b.beat)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|m| m.name.clone())
        .filter(|n| !n.is_empty())
}

/// Timeline → session: capture `[start_beat, end_beat)` across every leaf
/// non-audio layer into one scene, one slot per layer with clips in range.
///
/// `scene_id: None` appends a new scene (named from the nearest marker at or
/// before `start_beat`, else "Scene N"); `Some(id)` captures into an existing
/// scene, replacing any slot already at (layer, that scene).
#[derive(Debug)]
pub struct CaptureRangeToSceneCommand {
    start_beat: Beats,
    end_beat: Beats,
    requested_scene_id: Option<SceneId>,

    // Recorded during execute, for undo.
    created_scene_id: Option<SceneId>,
    /// Per touched (layer, scene) cell: the slot that was there before this
    /// command ran (`None` if the cell was empty).
    touched_slots: Vec<(LayerId, SceneId, Option<SessionSlot>)>,
}

impl CaptureRangeToSceneCommand {
    pub fn new(start_beat: Beats, end_beat: Beats, scene_id: Option<SceneId>) -> Self {
        Self {
            start_beat,
            end_beat,
            requested_scene_id: scene_id,
            created_scene_id: None,
            touched_slots: Vec::new(),
        }
    }
}

impl Command for CaptureRangeToSceneCommand {
    fn execute(&mut self, project: &mut Project) {
        self.created_scene_id = None;
        self.touched_slots.clear();

        let spb = project.settings.seconds_per_beat();
        let region = SelectionRegion {
            start_beat: self.start_beat,
            end_beat: self.end_beat,
            ..Default::default()
        };

        let scene_id = match &self.requested_scene_id {
            Some(id) => id.clone(),
            None => {
                let name = nearest_marker_name_at_or_before(project, self.start_beat)
                    .unwrap_or_else(|| format!("Scene {}", project.session.scenes.len() + 1));
                let scene = Scene {
                    id: SceneId::new(manifold_core::short_id()),
                    name,
                    color: None,
                };
                let id = scene.id.clone();
                project.session.scenes.push(scene);
                self.created_scene_id = Some(id.clone());
                id
            }
        };

        // Snapshot layer identities + clips up front: we only read the
        // timeline here (capture never mutates it), so this is a plain
        // iteration, not a borrow-split concern.
        for layer in &project.timeline.layers {
            if layer.is_group() || layer.layer_type == LayerType::Audio {
                continue;
            }

            let mut clips: Vec<TimelineClip> = layer
                .clips
                .iter()
                .filter(|c| c.end_beat() > self.start_beat && c.start_beat < self.end_beat)
                .map(|c| {
                    // Reuse the existing region-trim math verbatim (head/tail
                    // clamp + in_point advance for video clips) — see
                    // `EditingService::trim_clip_to_region`.
                    let mut trimmed = EditingService::trim_clip_to_region(c, &region, spb);
                    // Rebase from timeline-absolute to sequence-relative.
                    trimmed.start_beat -= self.start_beat;
                    trimmed
                })
                .collect();

            if clips.is_empty() {
                continue;
            }
            clips.sort_by(|a, b| {
                a.start_beat
                    .partial_cmp(&b.start_beat)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            let slot = SessionSlot {
                layer_id: layer.layer_id.clone(),
                scene_id: scene_id.clone(),
                sequence: ClipSequence {
                    length_beats: self.end_beat - self.start_beat,
                    clips,
                },
                name: String::new(),
                color: None,
            };

            let pos = project
                .session
                .slots
                .iter()
                .position(|s| s.layer_id == slot.layer_id && s.scene_id == slot.scene_id);
            let prior = pos.map(|i| project.session.slots.remove(i));
            self.touched_slots
                .push((slot.layer_id.clone(), slot.scene_id.clone(), prior));
            project.session.slots.push(slot);
        }

        project.session.mark_slot_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        for (layer_id, scene_id, prior) in self.touched_slots.drain(..).rev() {
            if let Some(i) = project
                .session
                .slots
                .iter()
                .position(|s| s.layer_id == layer_id && s.scene_id == scene_id)
            {
                project.session.slots.remove(i);
            }
            if let Some(prior_slot) = prior {
                project.session.slots.push(prior_slot);
            }
        }

        if let Some(created_id) = self.created_scene_id.take()
            && let Some(pos) = project
                .session
                .scenes
                .iter()
                .position(|s| s.id == created_id)
        {
            project.session.scenes.remove(pos);
        }

        project.session.mark_slot_lookup_dirty();
    }

    fn description(&self) -> &str {
        "Capture Range to Scene"
    }
}

/// Session → timeline: paste the slot at (layer, scene) into the layer's
/// lane, offset by `at_beat`. Reverse of `CaptureRangeToSceneCommand`.
#[derive(Debug)]
pub struct PasteSlotToTimelineCommand {
    layer_id: LayerId,
    scene_id: SceneId,
    at_beat: Beats,

    /// Per pasted clip: its fresh id + the overlap actions `Layer::add_clip`
    /// performed for it — mirrors `AddClipCommand`
    /// (`crates/manifold-editing/src/commands/clip.rs`), generalized to a
    /// whole sequence of clips instead of one.
    pasted: Vec<(ClipId, Vec<OverlapAction>)>,
}

impl PasteSlotToTimelineCommand {
    pub fn new(layer_id: LayerId, scene_id: SceneId, at_beat: Beats) -> Self {
        Self {
            layer_id,
            scene_id,
            at_beat,
            pasted: Vec::new(),
        }
    }
}

impl Command for PasteSlotToTimelineCommand {
    fn execute(&mut self, project: &mut Project) {
        self.pasted.clear();
        let spb = project.settings.seconds_per_beat();

        let sequence_clips: Vec<TimelineClip> =
            match project.session.get_slot(&self.layer_id, &self.scene_id) {
                Some(slot) => slot.sequence.clips.clone(),
                None => return,
            };

        let Some(li) = project.timeline.layer_index_for_id(&self.layer_id) else {
            return;
        };

        for clip in &sequence_clips {
            // Fresh identity both directions (§7) — never share ClipIds with
            // the slot's stored sequence.
            let mut new_clip = clip.clone_with_new_id();
            new_clip.start_beat += self.at_beat;
            let new_id = new_clip.id.clone();

            if let Some(layer) = project.timeline.layers.get_mut(li) {
                let actions = layer.add_clip(new_clip, spb);
                self.pasted.push((new_id, actions));
            }
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(li) = project.timeline.layer_index_for_id(&self.layer_id) else {
            return;
        };

        for (clip_id, actions) in self.pasted.drain(..).rev() {
            if let Some(layer) = project.timeline.layers.get_mut(li) {
                layer.remove_clip(&clip_id);

                for action in actions.iter().rev() {
                    match action {
                        OverlapAction::Deleted(c) => {
                            layer.restore_clip(c.clone());
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
                            layer.remove_clip(&tail_clip.id);
                            if let Some(c) = layer.find_clip_mut(clip_id) {
                                c.duration_beats = *old_duration_beats;
                            }
                        }
                    }
                }
                layer.mark_clips_unsorted();
            }
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn description(&self) -> &str {
        "Paste Session Slot to Timeline"
    }
}
