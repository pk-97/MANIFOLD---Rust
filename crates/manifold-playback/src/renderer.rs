use std::any::Any;
use manifold_core::clip::TimelineClip;

/// Abstraction over clip renderers (video player pool, generator renderer, etc.).
/// Port of C# IClipRenderer interface.
pub trait ClipRenderer: Any {
    fn can_handle(&self, clip: &TimelineClip) -> bool;
    fn start_clip(&mut self, clip: &TimelineClip, current_time: f32) -> bool;
    fn stop_clip(&mut self, clip_id: &str);
    fn release_all(&mut self);

    fn is_clip_ready(&self, clip_id: &str) -> bool;
    fn is_active(&self, clip_id: &str) -> bool;
    fn is_clip_playing(&self, clip_id: &str) -> bool;

    fn needs_prepare_phase(&self) -> bool;
    fn needs_drift_correction(&self) -> bool;
    fn needs_pending_pause(&self) -> bool;

    fn get_clip_playback_time(&self, clip_id: &str) -> f32;
    fn get_clip_media_length(&self, clip_id: &str) -> f32;

    fn resume_clip(&mut self, clip_id: &str);
    fn pause_clip(&mut self, clip_id: &str);
    fn seek_clip(&mut self, clip_id: &str, video_time: f32);
    fn set_clip_looping(&mut self, clip_id: &str, looping: bool);
    fn set_clip_playback_rate(&mut self, clip_id: &str, rate: f32);

    fn pre_render(&mut self, time: f32, beat: f32, dt: f32);
    fn resize(&mut self, width: i32, height: i32);

    /// Downcast support for typed renderer access from app layer.
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Stub renderer for testing. Tracks active clips without doing real rendering.
pub struct StubRenderer {
    active_clips: std::collections::HashMap<String, StubClipState>,
    is_generator: bool,
}

struct StubClipState {
    playing: bool,
    ready: bool,
    playback_time: f32,
    media_length: f32,
    looping: bool,
    playback_rate: f32,
}

impl StubRenderer {
    pub fn new_video() -> Self {
        Self { active_clips: std::collections::HashMap::new(), is_generator: false }
    }

    pub fn new_generator() -> Self {
        Self { active_clips: std::collections::HashMap::new(), is_generator: true }
    }
}

impl ClipRenderer for StubRenderer {
    fn can_handle(&self, clip: &TimelineClip) -> bool {
        if self.is_generator { clip.is_generator() } else { !clip.is_generator() }
    }

    fn start_clip(&mut self, clip: &TimelineClip, _current_time: f32) -> bool {
        self.active_clips.insert(clip.id.clone(), StubClipState {
            playing: true,
            ready: true,
            playback_time: 0.0,
            media_length: 10.0, // stub: 10 seconds
            looping: clip.is_looping,
            playback_rate: 1.0,
        });
        true
    }

    fn stop_clip(&mut self, clip_id: &str) {
        self.active_clips.remove(clip_id);
    }

    fn release_all(&mut self) {
        self.active_clips.clear();
    }

    fn is_clip_ready(&self, clip_id: &str) -> bool {
        self.active_clips.get(clip_id).is_some_and(|s| s.ready)
    }

    fn is_active(&self, clip_id: &str) -> bool {
        self.active_clips.contains_key(clip_id)
    }

    fn is_clip_playing(&self, clip_id: &str) -> bool {
        self.active_clips.get(clip_id).is_some_and(|s| s.playing)
    }

    fn needs_prepare_phase(&self) -> bool { !self.is_generator }
    fn needs_drift_correction(&self) -> bool { !self.is_generator }
    fn needs_pending_pause(&self) -> bool { !self.is_generator }

    fn get_clip_playback_time(&self, clip_id: &str) -> f32 {
        self.active_clips.get(clip_id).map_or(0.0, |s| s.playback_time)
    }

    fn get_clip_media_length(&self, clip_id: &str) -> f32 {
        self.active_clips.get(clip_id).map_or(0.0, |s| s.media_length)
    }

    fn resume_clip(&mut self, clip_id: &str) {
        if let Some(s) = self.active_clips.get_mut(clip_id) { s.playing = true; }
    }

    fn pause_clip(&mut self, clip_id: &str) {
        if let Some(s) = self.active_clips.get_mut(clip_id) { s.playing = false; }
    }

    fn seek_clip(&mut self, clip_id: &str, video_time: f32) {
        if let Some(s) = self.active_clips.get_mut(clip_id) { s.playback_time = video_time; }
    }

    fn set_clip_looping(&mut self, clip_id: &str, looping: bool) {
        if let Some(s) = self.active_clips.get_mut(clip_id) { s.looping = looping; }
    }

    fn set_clip_playback_rate(&mut self, clip_id: &str, rate: f32) {
        if let Some(s) = self.active_clips.get_mut(clip_id) { s.playback_rate = rate; }
    }

    fn pre_render(&mut self, _time: f32, _beat: f32, _dt: f32) {}
    fn resize(&mut self, _width: i32, _height: i32) {}

    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}
