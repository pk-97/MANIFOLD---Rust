use crate::command::Command;
use manifold_core::ClipId;
use manifold_core::project::Project;
use manifold_core::clip::TimelineClip;
use manifold_core::types::{GeneratorType, LayerType};

/// Move a clip to a new beat position and/or layer.
/// Matches Unity MoveClipCommand: cross-layer transfer removes from source and adds to target,
/// generator-type adoption when moving to a generator layer, and undo restores the original type.
#[derive(Debug)]
pub struct MoveClipCommand {
    clip_id: ClipId,
    old_start_beat: f32,
    new_start_beat: f32,
    old_layer_index: i32,
    new_layer_index: i32,
    /// Captured at construction time from the clip's current generator_type.
    /// Port of C# MoveClipCommand line 32: captures in constructor.
    old_generator_type: GeneratorType,
}

impl MoveClipCommand {
    /// Create a MoveClipCommand. `old_generator_type` captures the clip's generator type
    /// at the time the command is created (matching Unity constructor behavior).
    pub fn new_with_gen_type(clip_id: ClipId, old_start_beat: f32, new_start_beat: f32, old_layer_index: i32, new_layer_index: i32, old_generator_type: GeneratorType) -> Self {
        Self { clip_id, old_start_beat, new_start_beat, old_layer_index, new_layer_index, old_generator_type }
    }

    /// Convenience constructor that defaults generator type to None.
    /// For callers that know the clip isn't a generator or will look it up themselves.
    pub fn new(clip_id: ClipId, old_start_beat: f32, new_start_beat: f32, old_layer_index: i32, new_layer_index: i32) -> Self {
        Self { clip_id, old_start_beat, new_start_beat, old_layer_index, new_layer_index, old_generator_type: GeneratorType::None }
    }
}

impl Command for MoveClipCommand {
    fn execute(&mut self, project: &mut Project) {
        if self.old_layer_index != self.new_layer_index {
            let src = self.old_layer_index as usize;
            let dst = self.new_layer_index as usize;

            // Remove clip from source layer.
            let mut clip = if let Some(layer) = project.timeline.layers.get_mut(src) {
                layer.remove_clip(&self.clip_id)
            } else {
                None
            };

            if let Some(ref mut c) = clip {
                c.layer_index = self.new_layer_index;

                // Generator-type adoption: when target is a generator layer, adopt its type.
                if let Some(target) = project.timeline.layers.get(dst)
                    && target.layer_type == LayerType::Generator {
                        c.generator_type = target.generator_type();
                    }
            }

            // Add clip to target layer.
            if let (Some(c), Some(layer)) = (clip, project.timeline.layers.get_mut(dst)) {
                layer.add_clip(c);
            }
        } else {
            // Same-layer move: just update start_beat.
            if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
                clip.start_beat = self.new_start_beat;
            }
            if let Some(layer) = project.timeline.layers.get_mut(self.new_layer_index as usize) {
                layer.mark_clips_unsorted();
            }
            project.timeline.mark_clip_lookup_dirty();
            return;
        }

        // Update start_beat on the (now in target layer) clip.
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.start_beat = self.new_start_beat;
        }

        if let Some(layer) = project.timeline.layers.get_mut(self.new_layer_index as usize) {
            layer.mark_clips_unsorted();
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        if self.old_layer_index != self.new_layer_index {
            let src = self.new_layer_index as usize;
            let dst = self.old_layer_index as usize;

            // Remove clip from current (new) layer.
            let mut clip = if let Some(layer) = project.timeline.layers.get_mut(src) {
                layer.remove_clip(&self.clip_id)
            } else {
                None
            };

            if let Some(ref mut c) = clip {
                c.layer_index = self.old_layer_index;
            }

            // Add clip back to original layer.
            if let (Some(c), Some(layer)) = (clip, project.timeline.layers.get_mut(dst)) {
                layer.add_clip(c);
            }
        }

        // Restore generator type and start beat.
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.generator_type = self.old_generator_type;
            clip.start_beat = self.old_start_beat;
        }

        if let Some(layer) = project.timeline.layers.get_mut(self.old_layer_index as usize) {
            layer.mark_clips_unsorted();
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn description(&self) -> &str { "Move Clip" }
}

/// Trim a clip (change start beat, duration, and/or in-point).
/// Calls mark_clips_unsorted when StartBeat changes (matches Unity TrimClipCommand).
#[derive(Debug)]
pub struct TrimClipCommand {
    clip_id: ClipId,
    layer_index: Option<i32>,
    old_start_beat: f32,
    new_start_beat: f32,
    old_duration_beats: f32,
    new_duration_beats: f32,
    old_in_point: f32,
    new_in_point: f32,
}

impl TrimClipCommand {
    pub fn new(
        clip_id: ClipId,
        old_start_beat: f32, new_start_beat: f32,
        old_duration_beats: f32, new_duration_beats: f32,
        old_in_point: f32, new_in_point: f32,
    ) -> Self {
        Self { clip_id, layer_index: None, old_start_beat, new_start_beat, old_duration_beats, new_duration_beats, old_in_point, new_in_point }
    }
}

impl Command for TrimClipCommand {
    fn execute(&mut self, project: &mut Project) {
        // Capture layer_index on first execute for mark_clips_unsorted.
        if self.layer_index.is_none() {
            self.layer_index = project.timeline.find_clip_by_id(&self.clip_id)
                .map(|c| c.layer_index);
        }

        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.start_beat = self.new_start_beat;
            clip.duration_beats = self.new_duration_beats;
            clip.in_point = self.new_in_point;
        }

        if (self.old_start_beat - self.new_start_beat).abs() > f32::EPSILON
            && let Some(li) = self.layer_index
                && let Some(layer) = project.timeline.layers.get_mut(li as usize) {
                    layer.mark_clips_unsorted();
                }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.start_beat = self.old_start_beat;
            clip.duration_beats = self.old_duration_beats;
            clip.in_point = self.old_in_point;
        }

        if (self.old_start_beat - self.new_start_beat).abs() > f32::EPSILON
            && let Some(li) = self.layer_index
                && let Some(layer) = project.timeline.layers.get_mut(li as usize) {
                    layer.mark_clips_unsorted();
                }
    }

    fn description(&self) -> &str { "Trim Clip" }
}

/// Delete a clip from the timeline.
#[derive(Debug)]
pub struct DeleteClipCommand {
    clip: Option<TimelineClip>,
    layer_index: i32,
}

impl DeleteClipCommand {
    pub fn new(clip: TimelineClip, layer_index: i32) -> Self {
        Self { clip: Some(clip), layer_index }
    }
}

impl Command for DeleteClipCommand {
    fn execute(&mut self, project: &mut Project) {
        let clip_id = self.clip.as_ref().unwrap().id.clone();
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index as usize) {
            layer.remove_clip(&clip_id);
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = self.clip.clone() {
            if let Some(layer) = project.timeline.layers.get_mut(self.layer_index as usize) {
                layer.add_clip(clip);
            }
            project.timeline.mark_clip_lookup_dirty();
        }
    }

    fn description(&self) -> &str { "Delete Clip" }
}

/// Add a clip to the timeline.
#[derive(Debug)]
pub struct AddClipCommand {
    clip: TimelineClip,
    layer_index: i32,
}

impl AddClipCommand {
    pub fn new(clip: TimelineClip, layer_index: i32) -> Self {
        Self { clip, layer_index }
    }
}

impl Command for AddClipCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index as usize) {
            layer.add_clip(self.clip.clone());
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index as usize) {
            layer.remove_clip(&self.clip.id);
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn description(&self) -> &str { "Add Clip" }
}

/// Swap the video source of a clip.
#[derive(Debug)]
pub struct SwapVideoCommand {
    clip_id: ClipId,
    old_video_clip_id: String,
    new_video_clip_id: String,
    old_in_point: f32,
    new_in_point: f32,
    old_duration_beats: f32,
    new_duration_beats: f32,
}

impl SwapVideoCommand {
    pub fn new(
        clip_id: ClipId,
        old_video_clip_id: String, new_video_clip_id: String,
        old_in_point: f32, new_in_point: f32,
        old_duration_beats: f32, new_duration_beats: f32,
    ) -> Self {
        Self { clip_id, old_video_clip_id, new_video_clip_id, old_in_point, new_in_point, old_duration_beats, new_duration_beats }
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

    fn description(&self) -> &str { "Swap Video" }
}

/// Slip a clip's in-point without changing timeline position.
#[derive(Debug)]
pub struct SlipClipCommand {
    clip_id: ClipId,
    old_in_point: f32,
    new_in_point: f32,
}

impl SlipClipCommand {
    pub fn new(clip_id: ClipId, old_in_point: f32, new_in_point: f32) -> Self {
        Self { clip_id, old_in_point, new_in_point }
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

    fn description(&self) -> &str { "Slip Clip" }
}

/// Change clip visual effects (invert, loop, transform).
#[derive(Debug, Clone)]
pub struct ClipEffectsSnapshot {
    pub invert_colors: bool,
    pub is_looping: bool,
    pub loop_duration_beats: f32,
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
        clip.invert_colors = snap.invert_colors;
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

    fn description(&self) -> &str { "Change Clip Effects" }
}

/// Change clip loop settings.
#[derive(Debug)]
pub struct ChangeClipLoopCommand {
    clip_id: ClipId,
    old_looping: bool,
    new_looping: bool,
    old_loop_duration: f32,
    new_loop_duration: f32,
}

impl ChangeClipLoopCommand {
    pub fn new(clip_id: ClipId, old_looping: bool, new_looping: bool, old_loop_duration: f32, new_loop_duration: f32) -> Self {
        Self { clip_id, old_looping, new_looping, old_loop_duration, new_loop_duration }
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

    fn description(&self) -> &str { "Change Clip Loop" }
}

/// Change clip recorded BPM.
#[derive(Debug)]
pub struct ChangeClipRecordedBpmCommand {
    clip_id: ClipId,
    old_bpm: f32,
    new_bpm: f32,
}

impl ChangeClipRecordedBpmCommand {
    pub fn new(clip_id: ClipId, old_bpm: f32, new_bpm: f32) -> Self {
        Self { clip_id, old_bpm, new_bpm }
    }
}

impl Command for ChangeClipRecordedBpmCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.recorded_bpm = self.new_bpm;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.recorded_bpm = self.old_bpm;
        }
    }

    fn description(&self) -> &str { "Change Recorded BPM" }
}

/// Split a clip at a given beat, creating a tail clip.
#[derive(Debug)]
pub struct SplitClipCommand {
    clip_id: ClipId,
    layer_index: i32,
    old_duration_beats: f32,
    new_duration_beats: f32,
    tail_clip: TimelineClip,
}

impl SplitClipCommand {
    pub fn new(
        clip_id: ClipId,
        layer_index: i32,
        old_duration_beats: f32,
        new_duration_beats: f32,
        tail_clip: TimelineClip,
    ) -> Self {
        Self { clip_id, layer_index, old_duration_beats, new_duration_beats, tail_clip }
    }
}

impl Command for SplitClipCommand {
    fn execute(&mut self, project: &mut Project) {
        // Trim original
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.duration_beats = self.new_duration_beats;
        }
        // Add tail
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index as usize) {
            layer.add_clip(self.tail_clip.clone());
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        // Remove tail
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index as usize) {
            layer.remove_clip(&self.tail_clip.id);
        }
        // Restore original duration
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.duration_beats = self.old_duration_beats;
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn description(&self) -> &str { "Split Clip" }
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
        Self { clip_id, old_muted, new_muted }
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

    fn description(&self) -> &str { "Mute Clip" }
}
