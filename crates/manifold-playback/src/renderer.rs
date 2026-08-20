use manifold_core::ClipId;
use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::{Beats, Seconds};
use std::any::Any;

/// Abstraction over clip renderers (video player pool, generator renderer, etc.).
/// Port of C# IClipRenderer interface.
pub trait ClipRenderer: Any + Send {
    fn can_handle(&self, clip: &TimelineClip) -> bool;
    /// `fire_clip_edge=false` starts the clip without counting a clip edge
    /// (P3 heal: a layer-drag rebind is not a trigger). Renderers without an
    /// edge concept ignore it.
    fn start_clip(
        &mut self,
        clip: &TimelineClip,
        current_time: Seconds,
        layers: &[Layer],
        layer_index: i32,
        fire_clip_edge: bool,
    ) -> bool;
    fn stop_clip(&mut self, clip_id: &str);

    /// Drop ALL project-derived state: active clips, every cache keyed by
    /// project-local ids (`LayerId` / `ClipId`), pooled render targets.
    /// Called by `PlaybackEngine::initialize` at EVERY project boundary.
    /// This is not optional cleanup: ids and serialized version counters
    /// collide across projects derived from the same template, so any state
    /// that survives is stale the moment a new project arrives (BUG-256).
    fn release_all(&mut self);

    /// Called when a project is loaded/changed.
    /// Port of C# IClipRenderer.OnProjectLoaded (lines 16-17).
    fn on_project_loaded(&mut self, _project: &Project) {}

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

    fn pre_render(&mut self, time: Seconds, beat: Beats, dt: f32);
    fn resize(&mut self, width: i32, height: i32);

    /// True if any active clip has a decode job in-flight.
    fn has_pending_decodes(&self) -> bool {
        false
    }

    /// Block until all in-flight decode jobs complete and process results.
    /// No-op for renderers without async decode.
    fn flush_pending_decodes(&mut self) {}

    /// Per-frame RT quality column (RT_QUALITY_SETTINGS_DESIGN.md D5). The
    /// content pipeline resolves the active column (realtime vs export) once
    /// per frame and fans it out to every renderer; renderers without RT
    /// graphs ignore it. The column stays a foundation value type so this
    /// trait doesn't name renderer types — `GeneratorRenderer` converts via
    /// `RtQuality::from_column`.
    fn set_rt_quality(&mut self, _column: &manifold_core::settings::RtQualityColumn) {}

    /// Load-time warmup for one layer's renderer. Default no-op.
    /// Implementations that hold async state (generators with GLB/RT/DNN
    /// atoms) render the layer offscreen until quiescent or budget.
    fn prewarm_layer(
        &mut self,
        _layer: &Layer,
        _budget: manifold_core::WarmupBudget,
    ) -> manifold_core::WarmupOutcome {
        manifold_core::WarmupOutcome::Quiescent
    }

    /// Downcast support for typed renderer access from app layer.
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Stub renderer for testing. Tracks active clips without doing real rendering.
pub struct StubRenderer {
    active_clips: std::collections::HashMap<ClipId, StubClipState>,
    is_generator: bool,
    /// Test instrumentation (P3): every start, in order, with the edge flag it
    /// carried — lets tests prove a heal restarted a clip with no edge fired.
    start_log: Vec<(ClipId, bool)>,
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
        Self {
            active_clips: std::collections::HashMap::new(),
            is_generator: false,
            start_log: Vec::new(),
        }
    }

    pub fn new_generator() -> Self {
        Self {
            active_clips: std::collections::HashMap::new(),
            is_generator: true,
            start_log: Vec::new(),
        }
    }

    #[doc(hidden)]
    pub fn start_count_for(&self, clip_id: &str) -> usize {
        self.start_log
            .iter()
            .filter(|(id, _)| id.as_str() == clip_id)
            .count()
    }

    #[doc(hidden)]
    pub fn last_edge_flag_for(&self, clip_id: &str) -> Option<bool> {
        self.start_log
            .iter()
            .rev()
            .find(|(id, _)| id.as_str() == clip_id)
            .map(|(_, edge)| *edge)
    }
}

impl ClipRenderer for StubRenderer {
    fn can_handle(&self, clip: &TimelineClip) -> bool {
        if self.is_generator {
            clip.video_clip_id.is_empty()
        } else {
            !clip.video_clip_id.is_empty()
        }
    }

    fn start_clip(
        &mut self,
        clip: &TimelineClip,
        _current_time: Seconds,
        _layers: &[Layer],
        _layer_index: i32,
        fire_clip_edge: bool,
    ) -> bool {
        self.start_log.push((clip.id.clone(), fire_clip_edge));
        self.active_clips.insert(
            clip.id.clone(),
            StubClipState {
                playing: true,
                ready: true,
                playback_time: 0.0,
                media_length: 10.0, // stub: 10 seconds
                looping: clip.is_looping,
                playback_rate: 1.0,
            },
        );
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

    fn needs_prepare_phase(&self) -> bool {
        !self.is_generator
    }
    fn needs_drift_correction(&self) -> bool {
        !self.is_generator
    }
    fn needs_pending_pause(&self) -> bool {
        !self.is_generator
    }

    fn get_clip_playback_time(&self, clip_id: &str) -> f32 {
        self.active_clips
            .get(clip_id)
            .map_or(0.0, |s| s.playback_time)
    }

    fn get_clip_media_length(&self, clip_id: &str) -> f32 {
        self.active_clips
            .get(clip_id)
            .map_or(0.0, |s| s.media_length)
    }

    fn resume_clip(&mut self, clip_id: &str) {
        if let Some(s) = self.active_clips.get_mut(clip_id) {
            s.playing = true;
        }
    }

    fn pause_clip(&mut self, clip_id: &str) {
        if let Some(s) = self.active_clips.get_mut(clip_id) {
            s.playing = false;
        }
    }

    fn seek_clip(&mut self, clip_id: &str, video_time: f32) {
        if let Some(s) = self.active_clips.get_mut(clip_id) {
            s.playback_time = video_time;
        }
    }

    fn set_clip_looping(&mut self, clip_id: &str, looping: bool) {
        if let Some(s) = self.active_clips.get_mut(clip_id) {
            s.looping = looping;
        }
    }

    fn set_clip_playback_rate(&mut self, clip_id: &str, rate: f32) {
        if let Some(s) = self.active_clips.get_mut(clip_id) {
            s.playback_rate = rate;
        }
    }

    fn pre_render(&mut self, _time: Seconds, _beat: Beats, _dt: f32) {}
    fn resize(&mut self, _width: i32, _height: i32) {}

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
