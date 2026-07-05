use crate::command::Command;
use manifold_core::audio_clip_detection::AudioClipDetection;
use manifold_core::clip::TimelineClip;
use manifold_core::layer::OverlapAction;
use manifold_core::project::Project;
use manifold_core::{Beats, ClipId, LayerId, Seconds};
use std::collections::HashSet;

/// Move a clip to a new beat position and/or layer.
/// Matches Unity MoveClipCommand: cross-layer transfer removes from source and adds to target,
/// generator-type adoption when moving to a generator layer, and undo restores the original type.
#[derive(Debug)]
pub struct MoveClipCommand {
    clip_id: ClipId,
    old_start_beat: Beats,
    new_start_beat: Beats,
    old_layer_id: LayerId,
    new_layer_id: LayerId,
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
        }
    }
}

impl Command for MoveClipCommand {
    fn execute(&mut self, project: &mut Project) {
        if self.old_layer_id != self.new_layer_id {
            let src = project.timeline.layer_index_for_id(&self.old_layer_id);
            let dst = project.timeline.layer_index_for_id(&self.new_layer_id);

            // Remove clip from source layer.
            let clip = if let Some(src_idx) = src
                && let Some(layer) = project.timeline.layers.get_mut(src_idx)
            {
                layer.remove_clip(&self.clip_id)
            } else {
                None
            };

            // Restore clip to target layer (overlap handled by batch).
            if let Some(c) = clip
                && let Some(dst_idx) = dst
                && let Some(layer) = project.timeline.layers.get_mut(dst_idx)
            {
                layer.restore_clip(c);
            }
        } else {
            // Same-layer move: just update start_beat.
            if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
                clip.start_beat = self.new_start_beat;
            }
            if let Some(dst_idx) = project.timeline.layer_index_for_id(&self.new_layer_id)
                && let Some(layer) = project.timeline.layers.get_mut(dst_idx)
            {
                layer.mark_clips_unsorted();
            }
            project.timeline.mark_clip_lookup_dirty();
            return;
        }

        // Update start_beat on the (now in target layer) clip.
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.start_beat = self.new_start_beat;
        }

        if let Some(dst_idx) = project.timeline.layer_index_for_id(&self.new_layer_id)
            && let Some(layer) = project.timeline.layers.get_mut(dst_idx)
        {
            layer.mark_clips_unsorted();
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
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
    }

    fn description(&self) -> &str {
        "Move Clip"
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
        }
    }
}

impl Command for TrimClipCommand {
    fn execute(&mut self, project: &mut Project) {
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
    }

    fn undo(&mut self, project: &mut Project) {
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
    spb: f32,
    /// Clips protected from this add's own overlap enforcement pass —
    /// members of the same batch operation (e.g. the drag/nudge selection
    /// that produced this add via an overlap-split tail). Empty for a
    /// standalone add.
    ignore_ids: HashSet<ClipId>,
    /// Overlap actions performed during execute — reversed on undo.
    overlap_actions: Vec<OverlapAction>,
}

impl AddClipCommand {
    pub fn new(clip: TimelineClip, layer_id: LayerId, spb: f32) -> Self {
        Self {
            clip,
            layer_id,
            spb,
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
        spb: f32,
        ignore_ids: HashSet<ClipId>,
    ) -> Self {
        Self {
            clip,
            layer_id,
            spb,
            ignore_ids,
            overlap_actions: Vec::new(),
        }
    }
}

impl Command for AddClipCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(li) = project.timeline.layer_index_for_id(&self.layer_id)
            && let Some(layer) = project.timeline.layers.get_mut(li)
        {
            self.overlap_actions = layer.add_clip(self.clip.clone(), &self.ignore_ids, self.spb);
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
/// zero and clears `recorded_bpm` (the old song's BPM is a lie about the new
/// file), keeps `start_beat`/`duration_beats` untouched, and keeps the
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
        }
    }
}

impl Command for ReplaceAudioFileCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.audio_file_path = self.new_path.clone();
            clip.source_duration = self.new_source_duration;
            clip.in_point = Seconds::ZERO;
            clip.recorded_bpm = 0.0;
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

/// Change clip recorded BPM.
#[derive(Debug)]
pub struct ChangeClipRecordedBpmCommand {
    clip_id: ClipId,
    old_bpm: f32,
    new_bpm: f32,
    /// Audio clips rescale their timeline length when the clip BPM (warp)
    /// changes, holding the played source span constant (Ableton model). Captured
    /// on the first execute and restored on undo. Untouched for non-audio clips.
    old_duration: Option<Beats>,
    new_duration: Option<Beats>,
}

impl ChangeClipRecordedBpmCommand {
    pub fn new(clip_id: ClipId, old_bpm: f32, new_bpm: f32) -> Self {
        Self {
            clip_id,
            old_bpm,
            new_bpm,
            old_duration: None,
            new_duration: None,
        }
    }
}

impl Command for ChangeClipRecordedBpmCommand {
    fn execute(&mut self, project: &mut Project) {
        // Effective tempo a clip plays at: its own BPM when warp is on, else the
        // project tempo (warp off). The played source span is duration * 60/eff,
        // so to hold that span constant the duration scales by eff_new / eff_old.
        let project_bpm = project.settings.bpm.0;
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            let is_audio = clip.is_audio();
            if self.old_duration.is_none() {
                self.old_duration = Some(clip.duration_beats);
            }
            clip.recorded_bpm = self.new_bpm;
            if is_audio {
                let eff = |bpm: f32| if bpm > 0.0 { bpm } else { project_bpm };
                let (old_eff, new_eff) = (eff(self.old_bpm), eff(self.new_bpm));
                if let Some(old_dur) = self.old_duration
                    && old_eff > 0.0
                    && new_eff > 0.0
                {
                    let scaled = Beats(old_dur.0 * (new_eff / old_eff) as f64);
                    clip.set_duration_beats(scaled);
                    self.new_duration = Some(scaled);
                }
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.recorded_bpm = self.old_bpm;
            if clip.is_audio()
                && let Some(old_dur) = self.old_duration
            {
                clip.set_duration_beats(old_dur);
            }
        }
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
