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
