use manifold_core::types::{ClockAuthority, LayerType, PlaybackState, TempoPointSource};
use manifold_core::clip::TimelineClip;
use manifold_core::math::BeatQuantizer;
use manifold_core::project::Project;
use manifold_core::tempo::TempoMapConverter;

use crate::renderer::ClipRenderer;
use crate::scheduler::ClipScheduler;
use crate::active_window::ActiveTimelineClipWindow;
use crate::live_clip_manager::LiveClipManager;

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

    // Live clip manager (MIDI phantom clips)
    live_clip_manager: Option<LiveClipManager>,

    // Compositor
    compositor_dirty_deadline: f64,

    // Deferred sync flag — set by mark_sync_dirty(), consumed by tick/driver.
    // Port of C# PlaybackEngine.syncClipsDirty (lines 281-289).
    sync_clips_dirty: bool,

    // Live external tempo (set by driver from sync controllers each frame).
    // Port of C# PlaybackEngine lines 113-116.
    has_live_external_tempo: bool,
    live_external_tempo_bpm: f32,
    live_external_tempo_source: TempoPointSource,

    // Drift correction
    last_sync_time: f32,
    drift_correction_count: i32,

    // Clock state (for out-of-tick operations)
    last_realtime_now: f64,
    last_frame_count: i32,

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
            live_clip_manager: None,
            compositor_dirty_deadline: 0.0,
            sync_clips_dirty: false,
            has_live_external_tempo: false,
            live_external_tempo_bpm: 0.0,
            live_external_tempo_source: TempoPointSource::Unknown,
            last_sync_time: 0.0,
            drift_correction_count: 0,
            last_realtime_now: 0.0,
            last_frame_count: 0,
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
    pub fn live_clip_manager(&self) -> Option<&LiveClipManager> { self.live_clip_manager.as_ref() }
    pub fn live_clip_manager_mut(&mut self) -> Option<&mut LiveClipManager> { self.live_clip_manager.as_mut() }
    pub fn compositor_dirty_deadline(&self) -> f64 { self.compositor_dirty_deadline }

    // ─── Renderer access ───

    /// Replace a renderer at the given index (e.g., swap stub for real renderer after GPU init).
    pub fn replace_renderer(&mut self, index: usize, renderer: Box<dyn ClipRenderer>) {
        self.renderers[index] = renderer;
    }

    /// Split borrow: get renderers and project simultaneously.
    /// Needed because Rust can't borrow both `&mut self.renderers` and `&self.project`
    /// through a single `&mut self`.
    pub fn split_renderer_project(&mut self) -> (&mut Vec<Box<dyn ClipRenderer>>, Option<&Project>) {
        (&mut self.renderers, self.project.as_ref())
    }

    // ─── Lifecycle ───

    pub fn initialize(&mut self, project: Project) {
        self.project = Some(project);
        self.active_window.reset();
        self.current_time_double = 0.0;
        self.current_time = 0.0;
        self.current_beat = 0.0;
        self.last_sync_time = 0.0;
        self.drift_correction_count = 0;
        self.sync_clips_dirty = false;
        self.last_realtime_now = 0.0;
        self.last_frame_count = 0;
    }

    /// Set the LiveClipManager after construction. Must be called before first tick.
    /// Port of C# PlaybackEngine.SetLiveClipManager (line 351).
    pub fn set_live_clip_manager(&mut self, mgr: LiveClipManager) {
        self.live_clip_manager = Some(mgr);
    }

    /// Reset the active clip window index. Call after bulk clip operations (undo/redo).
    pub fn reset_active_clip_window(&mut self) {
        self.active_window.reset();
    }

    /// Update clock state for non-tick operations (Play, Stop, Seek).
    /// Port of C# PlaybackEngine.SetClock (lines 560-564).
    pub fn set_clock(&mut self, realtime_now: f64, frame_count: i32) {
        self.last_realtime_now = realtime_now;
        self.last_frame_count = frame_count;
    }

    // ─── Sync dirty flag ───

    /// Mark that clips need re-synchronization (deferred from MIDI events).
    /// Port of C# PlaybackEngine.MarkSyncDirty (line 442).
    pub fn mark_sync_dirty(&mut self) {
        self.sync_clips_dirty = true;
    }

    /// Consume the sync-dirty flag. Returns true and resets if set.
    /// Port of C# PlaybackEngine.ConsumeSyncDirty (lines 284-289).
    pub fn consume_sync_dirty(&mut self) -> bool {
        if !self.sync_clips_dirty { return false; }
        self.sync_clips_dirty = false;
        true
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

        // Sync clips at current position (start clips that should be active)
        self.sync_clips_to_time();

        // Resume paused clips that were pre-warmed during Stop/LoadProject
        if self.active_clip_renderers.len() > 0 {
            self.resume_ready_clips();
        }
    }

    pub fn stop(&mut self) {
        self.current_state = PlaybackState::Stopped;
        self.stop_all_clips();
        // Clear live clip manager
        if let Some(mgr) = &mut self.live_clip_manager {
            mgr.clear_all();
        }
        self.current_time_double = 0.0;
        self.current_time = 0.0;
        self.current_beat = 0.0;
        self.compositor_dirty_deadline = 0.0; // Force one more compositor update
        self.active_window.reset();
        self.sync_clips_dirty = false;
    }

    pub fn pause(&mut self) {
        if self.current_state != PlaybackState::Playing { return; }
        self.current_state = PlaybackState::Paused;
        // Pause only seekable clips (generators render procedurally each frame)
        self.pause_active_clips();
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

    /// Set time from an external sync source (NudgeTime path).
    /// Port of C# PlaybackEngine.NudgeTime (lines 519-525).
    pub fn nudge_time(&mut self, time: f32) {
        self.current_time_double = time as f64;
        self.current_time = time;
        self.update_beat_from_time();
        self.sync_project_bpm_from_current_beat();
    }

    /// Set time from a seek. Returns beat delta for feedback buffer clearing.
    /// Port of C# PlaybackEngine.SeekTo (lines 530-538).
    pub fn seek_to(&mut self, time: f32) -> f32 {
        let old_beat = self.current_beat;
        self.set_time(time.max(0.0) as f64);
        self.sync_project_bpm_from_current_beat();
        self.active_window.reset();

        // Clear live clips on large seek
        let beat_delta = (self.current_beat - old_beat).abs();
        // Note: live_clip_manager.clear_on_seek needs a stop callback.
        // The engine's stop_clip handles renderer cleanup, but we can't call it here
        // due to borrow conflict. Instead, collect IDs and stop after the borrow.
        if beat_delta > 1.0 {
            if let Some(mgr) = &mut self.live_clip_manager {
                let ids_to_stop: Vec<String> = mgr.live_slots_list()
                    .iter()
                    .map(|(_, c)| c.id.clone())
                    .collect();
                mgr.clear_all();
                for id in &ids_to_stop {
                    // Stop via renderer directly (not full stop_clip to avoid double-remove)
                    if let Some(renderer_idx) = self.active_clip_renderers.remove(id.as_str()) {
                        self.renderers[renderer_idx].stop_clip(id);
                    }
                    self.active_clip_ids.remove(id);
                }
            }
        }

        // Re-sync clips at new position
        if self.current_state != PlaybackState::Stopped {
            self.sync_clips_to_time();
        }

        beat_delta
    }

    // ─── Core tick ───

    /// Advance playback by one frame. Returns compositor instructions.
    /// Must not be called re-entrantly.
    /// Port of C# PlaybackEngine.Tick + PlaybackController.Update orchestration.
    pub fn tick(&mut self, ctx: TickContext) -> TickResult {
        if self.is_ticking {
            return TickResult::default();
        }
        self.is_ticking = true;

        if self.project.is_none() {
            self.is_ticking = false;
            return TickResult::default();
        }

        self.last_realtime_now = ctx.realtime_now;
        self.last_frame_count = ctx.frame_count;

        let result = if self.current_state == PlaybackState::Playing {
            self.tick_playing(ctx)
        } else {
            self.tick_non_playing(ctx)
        };

        self.is_ticking = false;
        result
    }

    /// Playing-state tick: advance time, sync clips, build compositor output.
    fn tick_playing(&mut self, ctx: TickContext) -> TickResult {
        // 1. Advance time (unless external sync source is clock authority)
        if !self.external_time_sync {
            let dt = ctx.dt_seconds * self.playback_speed as f64;
            self.advance_time(dt);
        }

        // 2. Sync project BPM to current beat position
        self.sync_project_bpm_from_current_beat();

        // 3. Consume sync-dirty flag (always sync during playback)
        self.consume_sync_dirty();

        // 4. Sync clips to current time (start/stop as needed)
        self.sync_clips_to_time();

        // 5. Evaluate modulation pipeline (LFO drivers + ADSR envelopes)
        // Port of C# DriverController.Update() [execution order 50]
        let modulation_dirty = if let Some(project) = &mut self.project {
            crate::modulation::evaluate_modulation(project, self.current_beat)
        } else {
            false
        };
        if modulation_dirty {
            self.mark_compositor_dirty(ctx.realtime_now);
        }

        // 6. Process pending pauses
        self.process_pending_pauses(ctx.realtime_now);

        // 7. Clear expired recently-started entries
        self.recently_started_times.retain(|_, &mut start_time| {
            ctx.realtime_now - start_time < RECENTLY_STARTED_TIME as f64
        });

        // 8. Build ready clips for compositor
        self.build_ready_clips_list();

        let compositor_dirty = !self.ready_clips_list.is_empty()
            || ctx.realtime_now < self.compositor_dirty_deadline;

        TickResult {
            ready_clips: self.ready_clips_list.clone(),
            compositor_dirty,
            should_clear_compositor: self.ready_clips_list.is_empty() && self.active_clip_renderers.is_empty(),
            should_clear_feedback_buffer: false,
        }
    }

    /// Non-playing tick: only sync if dirty, update compositor while deadline active.
    fn tick_non_playing(&mut self, ctx: TickContext) -> TickResult {
        // Paused/Stopped: sync only if dirty flag set (deferred MIDI events)
        if self.consume_sync_dirty() {
            self.sync_clips_to_time();
        }

        // Evaluate modulation pipeline even when stopped (for scrub preview / inspector)
        if let Some(project) = &mut self.project {
            if crate::modulation::evaluate_modulation(project, self.current_beat) {
                self.mark_compositor_dirty(ctx.realtime_now);
            }
        }

        // Process pending pauses (needed in all states for scrub preview)
        self.process_pending_pauses(ctx.realtime_now);

        // Build ready clips
        self.build_ready_clips_list();

        // Compositor dirty while deadline active or generators are active
        let has_generators = self.active_clip_renderers.iter().any(|(_, &idx)| {
            !self.renderers[idx].needs_prepare_phase()
        });
        let compositor_dirty = ctx.realtime_now < self.compositor_dirty_deadline || has_generators;

        TickResult {
            ready_clips: self.ready_clips_list.clone(),
            compositor_dirty,
            should_clear_compositor: self.ready_clips_list.is_empty()
                && self.active_clip_renderers.is_empty()
                && !self.has_pending_clip_state(),
            should_clear_feedback_buffer: false,
        }
    }

    /// Build the ready_clips_list from currently active clips.
    fn build_ready_clips_list(&mut self) {
        self.ready_clips_list.clear();
        for (clip_id, _) in &self.active_clip_renderers {
            if self.recently_started_times.contains_key(clip_id) {
                continue;
            }
            if let Some(clip) = self.find_timeline_clip_clone(clip_id) {
                self.ready_clips_list.push(clip);
            }
        }
        // Sort by layer index descending (back to front for compositing)
        self.ready_clips_list.sort_by(|a, b| b.layer_index.cmp(&a.layer_index));
    }

    /// Clone a clip by ID for the ready list. Needed because we can't hold refs across mutable ops.
    fn find_timeline_clip_clone(&self, clip_id: &str) -> Option<TimelineClip> {
        self.find_timeline_clip(clip_id).cloned()
    }

    /// Query timeline for active clips at current beat, populating timeline_active_scratch.
    /// Uses split borrows to avoid cloning the project.
    fn query_active_timeline_clips(&mut self) {
        // Step 1: ensure layer sort caches are up-to-date (needs &mut project)
        if let Some(p) = &mut self.project {
            p.timeline.ensure_layers_sorted();
        }

        // Step 2: query active clips (split borrow: &self.project + &mut self.timeline_active_scratch)
        self.timeline_active_scratch.clear();
        if let Some(project) = &self.project {
            let beat = self.current_beat;
            let active_indices = project.timeline.get_active_clips_at_beat_ref(beat);
            for (li, ci) in &active_indices {
                if let Some(clip) = project.timeline.layers.get(*li).and_then(|l| l.clips.get(*ci)) {
                    self.timeline_active_scratch.push(clip.clone());
                }
            }
        }
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
        // Notify live clip manager so it can track which live slots are still active.
        // Port of C# PlaybackEngine.StopClip → liveClipManager.NotifyClipStopped (line 684).
        if let Some(mgr) = &mut self.live_clip_manager {
            mgr.notify_clip_stopped(clip_id);
        }
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
    /// The heart of deterministic playback — idempotent.
    fn sync_clips_to_time(&mut self) {
        if self.project.is_none() {
            return;
        }

        self.query_active_timeline_clips();

        let bpm = self.project.as_ref().map(|p| p.settings.bpm).unwrap_or(120.0);
        let spb = 60.0_f32 / bpm.max(20.0);
        let min_remaining_beats = if spb > 0.0 {
            MIN_START_REMAINING_TIME / spb
        } else {
            MIN_START_REMAINING_TIME
        };

        let sync_result = self.scheduler.compute_sync(
            self.current_time,
            self.current_beat,
            &self.timeline_active_scratch,
            &[],  // live_slots — wired in Phase 3D
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

        if !sync_result.to_stop.is_empty() {
            self.compositor_dirty_deadline = self.compositor_dirty_deadline.max(0.0 + COMPOSITOR_DIRTY_TIME as f64);
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

    /// Get seconds-per-beat at a given beat. Checks live external tempo first.
    /// Port of C# PlaybackEngine.GetSecondsPerBeatAtBeat (lines 1372-1387).
    pub fn get_seconds_per_beat_at_beat(&mut self, beat: f32) -> f32 {
        // Priority 1: live external tempo (Link/MIDI Clock)
        if let Some((live_bpm, _)) = self.try_get_live_external_tempo() {
            return TempoMapConverter::seconds_per_beat_from_bpm(live_bpm);
        }
        // Priority 2: tempo map
        if let Some(project) = &mut self.project {
            let bpm = project.tempo_map.get_bpm_at_beat(beat, project.settings.bpm);
            TempoMapConverter::seconds_per_beat_from_bpm(bpm)
        } else {
            0.5 // fallback 120 BPM
        }
    }

    /// Get BPM at a given beat. Checks live external tempo first.
    /// Port of C# PlaybackEngine.GetBpmAtBeat (lines 1393-1401).
    pub fn get_bpm_at_beat(&mut self, beat: f32) -> f32 {
        if let Some((live_bpm, _)) = self.try_get_live_external_tempo() {
            return live_bpm;
        }
        if let Some(project) = &mut self.project {
            project.tempo_map.get_bpm_at_beat(beat, project.settings.bpm)
        } else {
            120.0
        }
    }

    pub fn get_timeline_fallback_bpm(&self) -> f32 {
        self.project.as_ref().map_or(120.0, |p| p.settings.bpm)
    }

    // ─── Live external tempo ───

    /// Set live external tempo state (called by driver from sync controllers each frame).
    /// Port of C# PlaybackEngine.SetLiveExternalTempo (lines 543-548).
    pub fn set_live_external_tempo(&mut self, has_live: bool, bpm: f32, source: TempoPointSource) {
        self.has_live_external_tempo = has_live;
        self.live_external_tempo_bpm = bpm;
        self.live_external_tempo_source = source;
    }

    /// Try to get live external tempo (Link or MIDI Clock only).
    /// Port of C# PlaybackEngine.TryGetLiveExternalTempo (lines 1404-1421).
    pub fn try_get_live_external_tempo(&self) -> Option<(f32, TempoPointSource)> {
        if !self.has_live_external_tempo {
            return None;
        }
        let authority = self.project.as_ref()
            .map(|p| p.settings.clock_authority)
            .unwrap_or(ClockAuthority::Internal);
        if authority != ClockAuthority::Link && authority != ClockAuthority::MidiClock {
            return None;
        }
        if self.live_external_tempo_bpm > 0.0 {
            Some((self.live_external_tempo_bpm, self.live_external_tempo_source))
        } else {
            None
        }
    }

    /// Sync project settings BPM to the tempo at current beat position.
    /// Quantizes to avoid sub-step jitter dirtying the save file.
    /// Port of C# PlaybackEngine.SyncProjectBpmFromCurrentBeat (lines 1598-1620).
    pub fn sync_project_bpm_from_current_beat(&mut self) {
        let live_tempo = self.try_get_live_external_tempo();
        if let Some(project) = &mut self.project {
            let bpm = if let Some((live_bpm, _)) = live_tempo {
                live_bpm
            } else if !project.tempo_map.points.is_empty() {
                project.tempo_map.get_bpm_at_beat(self.current_beat, project.settings.bpm)
            } else {
                project.settings.bpm
            };

            let bpm = bpm.clamp(20.0, 300.0);
            let q_bpm = BeatQuantizer::quantize_bpm(bpm);
            if (project.settings.bpm - q_bpm).abs() > BeatQuantizer::BPM_STEP * 0.5 {
                project.settings.bpm = q_bpm;
            }
        }
    }

    // ─── Transport helpers ───

    /// Resume all paused clips that are ready (for Play from paused/stopped).
    /// Port of C# PlaybackEngine.ResumeReadyClips (lines 1141-1155).
    pub fn resume_ready_clips(&mut self) {
        let clip_ids: Vec<String> = self.active_clip_renderers.keys().cloned().collect();
        for clip_id in &clip_ids {
            if let Some(&idx) = self.active_clip_renderers.get(clip_id.as_str()) {
                if self.renderers[idx].needs_prepare_phase()
                    && self.renderers[idx].is_clip_ready(clip_id)
                    && !self.renderers[idx].is_clip_playing(clip_id)
                    && !self.preparing_clips.contains(clip_id)
                {
                    self.renderers[idx].resume_clip(clip_id);
                }
            }
        }
    }

    /// Pause all active seekable clips (for transport Pause).
    /// Port of C# PlaybackEngine.PauseActiveClips (lines 1157-1168).
    pub fn pause_active_clips(&mut self) {
        let clip_ids: Vec<String> = self.active_clip_renderers.keys().cloned().collect();
        for clip_id in &clip_ids {
            if let Some(&idx) = self.active_clip_renderers.get(clip_id.as_str()) {
                if self.renderers[idx].needs_prepare_phase()
                    && self.renderers[idx].is_clip_playing(clip_id)
                {
                    self.renderers[idx].pause_clip(clip_id);
                }
            }
        }
    }

    /// Find a clip by ID — checks live slots first, then shared timeline lookup.
    /// Port of C# PlaybackEngine.FindTimelineClip (lines 1065-1074).
    pub fn find_timeline_clip(&self, clip_id: &str) -> Option<&TimelineClip> {
        if let Some(mgr) = &self.live_clip_manager {
            if let Some(clip) = mgr.find_live_clip(clip_id) {
                return Some(clip);
            }
        }
        // Timeline.find_clip_by_id requires &mut self for cache, so fall back to linear scan
        self.project.as_ref().and_then(|p| {
            for layer in &p.timeline.layers {
                for clip in &layer.clips {
                    if clip.id == clip_id {
                        return Some(clip);
                    }
                }
            }
            None
        })
    }

    /// True if any clips are pending (preparing, pausing, or recently started).
    /// Port of C# PlaybackEngine.HasPendingClipState (lines 307-309).
    pub fn has_pending_clip_state(&self) -> bool {
        !self.preparing_clips.is_empty()
            || !self.pending_pauses.is_empty()
            || !self.recently_started_times.is_empty()
    }
}
