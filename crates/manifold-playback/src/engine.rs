use manifold_core::types::{LayerType, PlaybackState};
use manifold_core::clip::TimelineClip;
use manifold_core::project::Project;
use manifold_core::tempo::TempoMapConverter;

use crate::renderer::ClipRenderer;
use crate::scheduler::ClipScheduler;
use crate::active_window::ActiveTimelineClipWindow;

use std::collections::{HashMap, HashSet};

// ─── Constants ───

pub const MIN_CLIP_PLAYBACK_RATE: f32 = 0.05;
pub const MAX_CLIP_PLAYBACK_RATE: f32 = 8.0;
pub const PENDING_PAUSE_DELAY: f32 = 0.1;
pub const RECENTLY_STARTED_TIME: f32 = 0.1;
pub const LIVE_RECENTLY_STARTED_TIME: f32 = 0.02;
pub const COMPOSITOR_DIRTY_TIME: f32 = 0.05;
pub const MIN_START_REMAINING_TIME: f32 = 0.02;

// ─── Engine I/O ───

/// Input context for a single engine tick.
#[derive(Debug, Clone, Copy, Default)]
pub struct TickContext {
    pub dt_seconds: f64,
    pub realtime_now: f64,
    pub pre_render_dt: f32,
    pub frame_count: i32,
}

/// Output of a single engine tick.
#[derive(Debug, Clone, Default)]
pub struct TickResult {
    pub ready_clips: Vec<TimelineClip>,
    pub compositor_dirty: bool,
    pub should_clear_compositor: bool,
    pub should_clear_feedback_buffer: bool,
}

// ─── Playback Engine ───

/// Engine-agnostic playback logic. No platform dependencies.
/// All time comes via TickContext. Uses std math, logging via delegate.
#[allow(clippy::type_complexity)]
pub struct PlaybackEngine {
    // Transport state
    current_state: PlaybackState,
    current_time_double: f64,
    current_time: f32,
    current_beat: f32,
    playback_speed: f32,
    is_recording: bool,
    external_time_sync: bool,

    // Project reference
    project: Option<Project>,

    // Renderers
    renderers: Vec<Box<dyn ClipRenderer>>,

    // Active clip tracking
    active_clip_renderers: HashMap<String, usize>,  // clip_id → renderer index
    active_clip_ids: HashSet<String>,
    preparing_clips: HashSet<String>,
    pending_pauses: HashMap<String, f64>,  // clip_id → pause deadline
    looping_clip_ids: HashSet<String>,
    recently_started_times: HashMap<String, f64>,  // clip_id → start realtime

    // Scheduling
    scheduler: ClipScheduler,
    active_window: ActiveTimelineClipWindow,

    // Compositor
    compositor_dirty_deadline: f64,

    // Drift correction
    last_sync_time: f32,
    drift_correction_count: i32,

    // Pre-allocated scratch buffers
    stop_buffer: Vec<String>,
    ready_clips_list: Vec<TimelineClip>,
    timeline_active_scratch: Vec<TimelineClip>,

    // Re-entrancy guard
    is_ticking: bool,

    // Logging (optional)
    pub log: Option<Box<dyn Fn(&str)>>,
    pub log_warning: Option<Box<dyn Fn(&str)>>,
}

impl PlaybackEngine {
    pub fn new(renderers: Vec<Box<dyn ClipRenderer>>) -> Self {
        Self {
            current_state: PlaybackState::Stopped,
            current_time_double: 0.0,
            current_time: 0.0,
            current_beat: 0.0,
            playback_speed: 1.0,
            is_recording: false,
            external_time_sync: false,
            project: None,
            renderers,
            active_clip_renderers: HashMap::with_capacity(32),
            active_clip_ids: HashSet::with_capacity(32),
            preparing_clips: HashSet::with_capacity(8),
            pending_pauses: HashMap::with_capacity(8),
            looping_clip_ids: HashSet::with_capacity(16),
            recently_started_times: HashMap::with_capacity(8),
            scheduler: ClipScheduler::new(),
            active_window: ActiveTimelineClipWindow::new(),
            compositor_dirty_deadline: 0.0,
            last_sync_time: 0.0,
            drift_correction_count: 0,
            stop_buffer: Vec::with_capacity(16),
            ready_clips_list: Vec::with_capacity(32),
            timeline_active_scratch: Vec::with_capacity(32),
            is_ticking: false,
            log: None,
            log_warning: None,
        }
    }

    // ─── Properties ───

    pub fn current_state(&self) -> PlaybackState { self.current_state }
    pub fn current_time_double(&self) -> f64 { self.current_time_double }
    pub fn current_time(&self) -> f32 { self.current_time }
    pub fn current_beat(&self) -> f32 { self.current_beat }
    pub fn playback_speed(&self) -> f32 { self.playback_speed }
    pub fn is_playing(&self) -> bool { self.current_state == PlaybackState::Playing }
    pub fn is_recording(&self) -> bool { self.is_recording }
    pub fn external_time_sync(&self) -> bool { self.external_time_sync }
    pub fn active_clip_count(&self) -> usize { self.active_clip_renderers.len() }
    pub fn project(&self) -> Option<&Project> { self.project.as_ref() }
    pub fn project_mut(&mut self) -> Option<&mut Project> { self.project.as_mut() }

    // ─── Lifecycle ───

    pub fn initialize(&mut self, project: Project) {
        self.project = Some(project);
        self.active_window.reset();
        self.current_time_double = 0.0;
        self.current_time = 0.0;
        self.current_beat = 0.0;
        self.last_sync_time = 0.0;
        self.drift_correction_count = 0;
    }

    pub fn shutdown(&mut self) {
        self.stop_all_clips();
        self.project = None;
    }

    // ─── Transport ───

    pub fn set_state(&mut self, state: PlaybackState) {
        self.current_state = state;
    }

    pub fn play(&mut self) {
        if self.current_state == PlaybackState::Playing { return; }
        self.current_state = PlaybackState::Playing;
        self.pending_pauses.clear();
        self.sync_clips_to_time();
    }

    pub fn stop(&mut self) {
        self.current_state = PlaybackState::Stopped;
        self.stop_all_clips();
        self.current_time_double = 0.0;
        self.current_time = 0.0;
        self.current_beat = 0.0;
        self.compositor_dirty_deadline = 0.0; // Force one more compositor update
        self.active_window.reset();
    }

    pub fn pause(&mut self) {
        if self.current_state != PlaybackState::Playing { return; }
        self.current_state = PlaybackState::Paused;
        // Pause active video clips (generators keep rendering)
        let clip_ids: Vec<String> = self.active_clip_renderers.keys().cloned().collect();
        for clip_id in &clip_ids {
            if let Some(&renderer_idx) = self.active_clip_renderers.get(clip_id.as_str()) {
                self.renderers[renderer_idx].pause_clip(clip_id);
            }
        }
    }

    pub fn set_time(&mut self, time_double: f64) {
        self.current_time_double = time_double;
        self.current_time = time_double as f32;
        self.update_beat_from_time();
    }

    pub fn set_beat(&mut self, beat: f32) {
        self.current_beat = beat;
    }

    pub fn set_playback_speed(&mut self, speed: f32) {
        self.playback_speed = speed.clamp(MIN_CLIP_PLAYBACK_RATE, MAX_CLIP_PLAYBACK_RATE);
    }

    pub fn set_external_time_sync(&mut self, value: bool) {
        self.external_time_sync = value;
    }

    pub fn set_recording(&mut self, value: bool) {
        self.is_recording = value;
    }

    pub fn advance_time(&mut self, dt_seconds: f64) -> f32 {
        self.current_time_double += dt_seconds;
        self.current_time = self.current_time_double as f32;
        self.update_beat_from_time();
        self.current_time
    }

    pub fn nudge_time(&mut self, time: f32) {
        self.current_time_double = time as f64;
        self.current_time = time;
        self.update_beat_from_time();
    }

    pub fn seek_to(&mut self, time: f32) -> f32 {
        let old_beat = self.current_beat;
        self.set_time(time.max(0.0) as f64);
        self.active_window.reset();
        // Re-sync clips at new position
        if self.current_state != PlaybackState::Stopped {
            self.sync_clips_to_time();
        }
        self.current_beat - old_beat
    }

    // ─── Core tick ───

    pub fn tick(&mut self, ctx: TickContext) -> TickResult {
        if self.is_ticking {
            return TickResult::default();
        }
        self.is_ticking = true;

        let project = match &self.project {
            Some(p) => p.clone(), // TODO: avoid clone, use indices
            None => {
                self.is_ticking = false;
                return TickResult::default();
            }
        };

        // Advance time if playing
        if self.current_state == PlaybackState::Playing && !self.external_time_sync {
            let dt = ctx.dt_seconds * self.playback_speed as f64;
            self.advance_time(dt);
        }

        // Get active clips from timeline
        self.timeline_active_scratch.clear();
        let active_indices = {
            let mut timeline = project.timeline.clone();
            timeline.get_active_clips_at_beat(self.current_beat)
        };
        for (li, ci) in &active_indices {
            if let Some(clip) = project.timeline.layers.get(*li).and_then(|l| l.clips.get(*ci)) {
                self.timeline_active_scratch.push(clip.clone());
            }
        }

        // Compute dynamic min remaining beats from current BPM (Fix 1)
        let spb = 60.0_f32 / project.settings.bpm.max(20.0);
        let min_remaining_beats = if spb > 0.0 {
            MIN_START_REMAINING_TIME / spb
        } else {
            MIN_START_REMAINING_TIME
        };

        // Run scheduler (Fix 2: pass looping_clip_ids for bypass)
        // TODO Phase 3: pass live_clip_manager.live_slots_list() instead of empty slice
        let sync_result = self.scheduler.compute_sync(
            self.current_time,
            self.current_beat,
            &self.timeline_active_scratch,
            &[],  // live_slots — wired in Phase 3
            &self.active_clip_ids,
            &self.looping_clip_ids,
            min_remaining_beats,
        );

        // Stop clips
        for clip_id in &sync_result.to_stop {
            self.stop_clip(clip_id);
        }

        // Start clips
        for clip in &sync_result.to_start {
            self.start_clip(clip, ctx.realtime_now);
        }

        // Process pending pauses
        self.process_pending_pauses(ctx.realtime_now);

        // Clear expired recently-started entries
        self.recently_started_times.retain(|_, &mut start_time| {
            ctx.realtime_now - start_time < RECENTLY_STARTED_TIME as f64
        });

        // Build ready clips
        self.ready_clips_list.clear();
        for (li, ci) in &active_indices {
            if let Some(clip) = project.timeline.layers.get(*li).and_then(|l| l.clips.get(*ci)) {
                if self.active_clip_ids.contains(&clip.id) && !self.recently_started_times.contains_key(&clip.id) {
                    self.ready_clips_list.push(clip.clone());
                }
            }
        }

        // Sort by layer index descending (back to front)
        self.ready_clips_list.sort_by(|a, b| b.layer_index.cmp(&a.layer_index));

        let compositor_dirty = !self.ready_clips_list.is_empty()
            || ctx.realtime_now < self.compositor_dirty_deadline;

        let result = TickResult {
            ready_clips: self.ready_clips_list.clone(),
            compositor_dirty,
            should_clear_compositor: self.ready_clips_list.is_empty() && self.active_clip_renderers.is_empty(),
            should_clear_feedback_buffer: false,
        };

        self.is_ticking = false;
        result
    }

    // ─── Clip lifecycle ───

    pub fn start_clip(&mut self, clip: &TimelineClip, realtime_now: f64) {
        // Fix 6: Never start clips on group layers
        if let Some(project) = &self.project {
            if let Some(layer) = project.timeline.layers.get(clip.layer_index as usize) {
                if layer.layer_type == LayerType::Group {
                    return;
                }
            }
        }

        // Find renderer
        let renderer_idx = self.renderers.iter().position(|r| r.can_handle(clip));
        if let Some(idx) = renderer_idx {
            let success = self.renderers[idx].start_clip(clip, self.current_time);
            if success {
                self.active_clip_renderers.insert(clip.id.clone(), idx);
                self.active_clip_ids.insert(clip.id.clone());
                self.recently_started_times.insert(clip.id.clone(), realtime_now);

                if clip.is_looping {
                    self.looping_clip_ids.insert(clip.id.clone());
                }

                // Pending pause for video renderers
                if self.renderers[idx].needs_pending_pause() {
                    self.pending_pauses.insert(
                        clip.id.clone(),
                        realtime_now + PENDING_PAUSE_DELAY as f64,
                    );
                }

                self.mark_compositor_dirty(realtime_now);
            }
        }
    }

    pub fn stop_clip(&mut self, clip_id: &str) {
        if let Some(renderer_idx) = self.active_clip_renderers.remove(clip_id) {
            self.renderers[renderer_idx].stop_clip(clip_id);
        }
        self.active_clip_ids.remove(clip_id);
        self.preparing_clips.remove(clip_id);
        self.pending_pauses.remove(clip_id);
        self.looping_clip_ids.remove(clip_id);
        self.recently_started_times.remove(clip_id);
        // Fix 5: notify live clip manager when implemented
        // self.live_clip_manager.notify_clip_stopped(clip_id);
    }

    pub fn stop_all_clips(&mut self) {
        self.stop_buffer.clear();
        self.stop_buffer.extend(self.active_clip_ids.iter().cloned());
        for clip_id in &self.stop_buffer {
            if let Some(renderer_idx) = self.active_clip_renderers.remove(clip_id.as_str()) {
                self.renderers[renderer_idx].stop_clip(clip_id);
            }
        }
        self.active_clip_ids.clear();
        self.preparing_clips.clear();
        self.pending_pauses.clear();
        self.looping_clip_ids.clear();
        self.recently_started_times.clear();
    }

    // ─── Pending pauses ───

    fn process_pending_pauses(&mut self, realtime_now: f64) {
        let expired: Vec<String> = self.pending_pauses.iter()
            .filter(|(_, &deadline)| realtime_now >= deadline)
            .map(|(id, _)| id.clone())
            .collect();

        let had_expired = !expired.is_empty();

        for clip_id in expired {
            self.pending_pauses.remove(&clip_id);
            if let Some(&renderer_idx) = self.active_clip_renderers.get(&clip_id) {
                // Only pause if we're not playing
                if self.current_state != PlaybackState::Playing {
                    self.renderers[renderer_idx].pause_clip(&clip_id);
                }
            }
        }

        // Fix 4: Set compositor dirty after processing pending pauses
        if had_expired {
            self.compositor_dirty_deadline = realtime_now + COMPOSITOR_DIRTY_TIME as f64;
        }
    }

    // ─── Compositor ───

    pub fn mark_compositor_dirty(&mut self, realtime_now: f64) {
        self.compositor_dirty_deadline = realtime_now + COMPOSITOR_DIRTY_TIME as f64;
    }

    // ─── Sync ───

    /// Re-synchronize active clips to current playback position.
    /// Called by play() and seek_to() for immediate state consistency.
    fn sync_clips_to_time(&mut self) {
        let project = match &self.project {
            Some(p) => p.clone(),
            None => return,
        };

        self.timeline_active_scratch.clear();
        let active_indices = {
            let mut timeline = project.timeline.clone();
            timeline.get_active_clips_at_beat(self.current_beat)
        };
        for (li, ci) in &active_indices {
            if let Some(clip) = project.timeline.layers.get(*li).and_then(|l| l.clips.get(*ci)) {
                self.timeline_active_scratch.push(clip.clone());
            }
        }

        let spb = 60.0_f32 / project.settings.bpm.max(20.0);
        let min_remaining_beats = if spb > 0.0 {
            MIN_START_REMAINING_TIME / spb
        } else {
            MIN_START_REMAINING_TIME
        };

        // TODO Phase 3: pass live_clip_manager.live_slots_list() instead of empty slice
        let sync_result = self.scheduler.compute_sync(
            self.current_time,
            self.current_beat,
            &self.timeline_active_scratch,
            &[],  // live_slots — wired in Phase 3
            &self.active_clip_ids,
            &self.looping_clip_ids,
            min_remaining_beats,
        );

        for clip_id in &sync_result.to_stop {
            self.stop_clip(clip_id);
        }

        // Use realtime 0.0 as fallback since this is called outside tick context
        for clip in &sync_result.to_start {
            self.start_clip(clip, 0.0);
        }
    }

    // ─── Time/Tempo math ───

    fn update_beat_from_time(&mut self) {
        if let Some(project) = &mut self.project {
            self.current_beat = TempoMapConverter::seconds_to_beat(
                &mut project.tempo_map,
                self.current_time,
                project.settings.bpm,
            );
        }
    }

    pub fn time_to_timeline_beat(&mut self, time_seconds: f32) -> f32 {
        if let Some(project) = &mut self.project {
            TempoMapConverter::seconds_to_beat(
                &mut project.tempo_map,
                time_seconds,
                project.settings.bpm,
            )
        } else {
            time_seconds * 2.0 // fallback: 120 bpm
        }
    }

    pub fn beat_to_timeline_time(&mut self, beat: f32) -> f32 {
        if let Some(project) = &mut self.project {
            TempoMapConverter::beat_to_seconds(
                &mut project.tempo_map,
                beat,
                project.settings.bpm,
            )
        } else {
            beat * 0.5 // fallback: 120 bpm
        }
    }

    pub fn get_seconds_per_beat(&mut self) -> f32 {
        self.get_seconds_per_beat_at_beat(self.current_beat)
    }

    pub fn get_seconds_per_beat_at_beat(&mut self, beat: f32) -> f32 {
        if let Some(project) = &mut self.project {
            let bpm = project.tempo_map.get_bpm_at_beat(beat, project.settings.bpm);
            TempoMapConverter::seconds_per_beat_from_bpm(bpm)
        } else {
            0.5
        }
    }

    pub fn get_bpm_at_beat(&mut self, beat: f32) -> f32 {
        if let Some(project) = &mut self.project {
            project.tempo_map.get_bpm_at_beat(beat, project.settings.bpm)
        } else {
            120.0
        }
    }

    pub fn get_timeline_fallback_bpm(&self) -> f32 {
        self.project.as_ref().map_or(120.0, |p| p.settings.bpm)
    }
}
