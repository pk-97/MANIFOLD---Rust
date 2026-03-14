use crate::command::Command;
use manifold_core::project::Project;
use manifold_core::clip::TimelineClip;

/// Move a clip to a new beat position and/or layer.
#[derive(Debug)]
pub struct MoveClipCommand {
    clip_id: String,
    old_start_beat: f32,
    new_start_beat: f32,
    old_layer_index: i32,
    new_layer_index: i32,
}

impl MoveClipCommand {
    pub fn new(clip_id: String, old_start_beat: f32, new_start_beat: f32, old_layer_index: i32, new_layer_index: i32) -> Self {
        Self { clip_id, old_start_beat, new_start_beat, old_layer_index, new_layer_index }
    }
}

impl Command for MoveClipCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.start_beat = self.new_start_beat;
            clip.layer_index = self.new_layer_index;
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.start_beat = self.old_start_beat;
            clip.layer_index = self.old_layer_index;
        }
        project.timeline.mark_clip_lookup_dirty();
    }

    fn description(&self) -> &str { "Move Clip" }
}

/// Trim a clip (change start beat, duration, and/or in-point).
#[derive(Debug)]
pub struct TrimClipCommand {
    clip_id: String,
    old_start_beat: f32,
    new_start_beat: f32,
    old_duration_beats: f32,
    new_duration_beats: f32,
    old_in_point: f32,
    new_in_point: f32,
}

impl TrimClipCommand {
    pub fn new(
        clip_id: String,
        old_start_beat: f32, new_start_beat: f32,
        old_duration_beats: f32, new_duration_beats: f32,
        old_in_point: f32, new_in_point: f32,
    ) -> Self {
        Self { clip_id, old_start_beat, new_start_beat, old_duration_beats, new_duration_beats, old_in_point, new_in_point }
    }
}

impl Command for TrimClipCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.start_beat = self.new_start_beat;
            clip.duration_beats = self.new_duration_beats;
            clip.in_point = self.new_in_point;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.start_beat = self.old_start_beat;
            clip.duration_beats = self.old_duration_beats;
            clip.in_point = self.old_in_point;
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
    clip_id: String,
    old_video_clip_id: String,
    new_video_clip_id: String,
    old_in_point: f32,
    new_in_point: f32,
    old_duration_beats: f32,
    new_duration_beats: f32,
}

impl SwapVideoCommand {
    pub fn new(
        clip_id: String,
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
    clip_id: String,
    old_in_point: f32,
    new_in_point: f32,
}

impl SlipClipCommand {
    pub fn new(clip_id: String, old_in_point: f32, new_in_point: f32) -> Self {
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
    clip_id: String,
    old: ClipEffectsSnapshot,
    new: ClipEffectsSnapshot,
}

impl ClipEffectsCommand {
    pub fn new(clip_id: String, old: ClipEffectsSnapshot, new: ClipEffectsSnapshot) -> Self {
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
    clip_id: String,
    old_looping: bool,
    new_looping: bool,
    old_loop_duration: f32,
    new_loop_duration: f32,
}

impl ChangeClipLoopCommand {
    pub fn new(clip_id: String, old_looping: bool, new_looping: bool, old_loop_duration: f32, new_loop_duration: f32) -> Self {
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
    clip_id: String,
    old_bpm: f32,
    new_bpm: f32,
}

impl ChangeClipRecordedBpmCommand {
    pub fn new(clip_id: String, old_bpm: f32, new_bpm: f32) -> Self {
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
    clip_id: String,
    layer_index: i32,
    old_duration_beats: f32,
    new_duration_beats: f32,
    tail_clip: TimelineClip,
}

impl SplitClipCommand {
    pub fn new(
        clip_id: String,
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
    clip_id: String,
    old_muted: bool,
    new_muted: bool,
}

impl MuteClipCommand {
    pub fn new(clip_id: String, old_muted: bool, new_muted: bool) -> Self {
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
