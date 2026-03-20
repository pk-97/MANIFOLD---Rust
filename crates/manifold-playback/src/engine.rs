use manifold_core::layer::Layer;
use manifold_core::types::{ClockAuthority, GeneratorType, LayerType, PlaybackState, TempoPointSource};
use manifold_core::clip::TimelineClip;
use manifold_core::math::BeatQuantizer;
use manifold_core::project::Project;
use manifold_core::tempo::TempoMapConverter;

use crate::renderer::ClipRenderer;
use crate::scheduler::ClipScheduler;
use crate::active_window::ActiveTimelineClipWindow;
use crate::live_clip_manager::LiveClipManager;

use std::collections::{HashMap, HashSet};

// ─── Playback notification trait ───

/// Callback interface for playback events that affect the compositor/UI.
/// Port of C# IPlaybackNotifier.cs lines 9-18.
pub trait PlaybackNotifier {
    fn mark_compositor_dirty(&mut self);
    fn notify_generator_type_changed(&mut self, layer: &Layer, new_type: GeneratorType);
}

// ─── Constants ───

pub const MIN_CLIP_PLAYBACK_RATE: f32 = 0.05;
pub const MAX_CLIP_PLAYBACK_RATE: f32 = 8.0;
pub const PENDING_PAUSE_DELAY: f32 = 0.1;
pub const RECENTLY_STARTED_TIME: f32 = 0.1;
pub const LIVE_RECENTLY_STARTED_TIME: f32 = 0.02;
pub const COMPOSITOR_DIRTY_TIME: f32 = 0.05;
pub const MIN_START_REMAINING_TIME: f32 = 0.02;

// Lookahead pre-warm constants. Port of C# PlaybackEngine lines 84-98.
pub const LOOKAHEAD_PREWARM_AHEAD_TIME: f32 = 8.0;
pub const LOOKAHEAD_PREWARM_BEHIND_TIME: f32 = 0.25;
pub const LOOKAHEAD_PREWARM_INTERVAL: f32 = 0.5;
pub const LIVE_PREWARM_INTERVAL: f32 = 0.1;
pub const LIVE_PREWARM_BURST_TIME: f32 = 3.0;
pub const LOOKAHEAD_PREWARM_MAX_UNIQUE_CLIPS: usize = 12;
pub const LIVE_PREWARM_MAX_UNIQUE_CLIPS: usize = 12;
pub const LIVE_PREWARM_RECENT_PRIORITY_COUNT: usize = 4;
pub const COMBINED_PREWARM_MAX_UNIQUE_CLIPS: usize = 20;

// ─── Engine I/O ───

/// Input context for a single engine tick.
#[derive(Debug, Clone, Copy, Default)]
pub struct TickContext {
    pub dt_seconds: f64,
    pub realtime_now: f64,
    pub pre_render_dt: f32,
    pub frame_count: i32,
    /// Fixed delta for export mode (0.0 = use real dt_seconds).
    /// Port of C# PlaybackController.exportFixedDeltaSeconds (line 42).
    pub export_fixed_dt: f64,
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

    // Drift correction. Port of C# PlaybackController.videoSyncInterval (line 33).
    video_sync_interval: f32,
    last_sync_time: f32,
    drift_correction_count: i32,
    is_export_mode: bool,

    // Clock state (for out-of-tick operations)
    last_realtime_now: f64,
    last_frame_count: i32,

    // Pre-allocated scratch buffers
    stop_buffer: Vec<String>,
    ready_clips_list: Vec<TimelineClip>,
    timeline_active_scratch: Vec<TimelineClip>,
    became_ready_list: Vec<String>,
    clips_to_stop_drift: Vec<String>,
    prewarm_candidates: Vec<TimelineClip>,
    compositor_fallback_clips: Vec<TimelineClip>,

    // Prewarm state. Port of C# PlaybackEngine prewarm fields.
    next_prewarm_at: f64,
    last_prewarm_ids: HashSet<String>,

    // Re-entrancy guard
    is_ticking: bool,

    // Logging (optional). Port of C# PlaybackEngine lines 248-253.
    pub log: Option<Box<dyn Fn(&str) + Send>>,
    pub log_warning: Option<Box<dyn Fn(&str) + Send>>,
    pub log_error: Option<Box<dyn Fn(&str) + Send>>,

    // Callback delegates. Port of C# PlaybackEngine lines 254-275.
    pub replenish_warm_cache: Option<Box<dyn Fn(&[TimelineClip]) + Send>>,
    pub on_drift_corrected: Option<Box<dyn Fn(&str, f32) + Send>>,
    pub beat_snapped_beat_resolver: Option<Box<dyn Fn() -> f32 + Send>>,
    pub absolute_tick_resolver: Option<Box<dyn Fn() -> i32 + Send>>,
    pub record_command_delegate: Option<Box<dyn Fn(Box<dyn manifold_editing::command::Command>) + Send>>,

    // Debug flag. Port of C# PlaybackEngine.showDebugLogs.
    pub show_debug_logs: bool,

    // Callback: fires each frame during playback after AdvanceTime.
    // Port of C# PlaybackController.OnTimeChanged (line 1149).
    pub on_time_changed: Option<Box<dyn Fn(f32) + Send>>,

    // Cached media length resolver. Port of C# PlaybackEngine line 201.
    cached_get_media_length: HashMap<String, f32>,

    // Sort comparator scratch. Port of C# PlaybackEngine lines 211-214.
    // Rust uses closures for sorting — no static delegates needed, but
    // we keep the scratch buffers for zero-alloc iteration.
    to_pause_list: Vec<String>,
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
            video_sync_interval: 2.0,
            last_sync_time: 0.0,
            drift_correction_count: 0,
            is_export_mode: false,
            last_realtime_now: 0.0,
            last_frame_count: 0,
            stop_buffer: Vec::with_capacity(16),
            ready_clips_list: Vec::with_capacity(32),
            timeline_active_scratch: Vec::with_capacity(32),
            became_ready_list: Vec::with_capacity(8),
            clips_to_stop_drift: Vec::with_capacity(8),
            prewarm_candidates: Vec::with_capacity(32),
            compositor_fallback_clips: Vec::with_capacity(32),
            next_prewarm_at: 0.0,
            last_prewarm_ids: HashSet::with_capacity(16),
            is_ticking: false,
            log: None,
            log_warning: None,
            log_error: None,
            replenish_warm_cache: None,
            on_drift_corrected: None,
            beat_snapped_beat_resolver: None,
            absolute_tick_resolver: None,
            record_command_delegate: None,
            show_debug_logs: false,
            on_time_changed: None,
            cached_get_media_length: HashMap::with_capacity(32),
            to_pause_list: Vec::with_capacity(8),
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
    pub fn is_export_mode(&self) -> bool { self.is_export_mode }
    pub fn video_sync_interval(&self) -> f32 { self.video_sync_interval }
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

    pub fn set_export_mode(&mut self, value: bool) {
        self.is_export_mode = value;
    }

    pub fn set_video_sync_interval(&mut self, interval: f32) {
        self.video_sync_interval = interval;
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

        // Re-sync clips at new position — unconditional, matching Unity's
        // PlaybackController.Seek() which always calls SyncClipsToTime() + SeekActiveClips()
        // regardless of playback state. This is what makes scrub-while-stopped work.
        self.sync_clips_to_time();

        // Mark compositor dirty so the stopped-state tick renders the new frame.
        // Port of Unity SeekActiveClips() setting compositorDirtyDeadline.
        self.compositor_dirty_deadline = self.last_realtime_now + COMPOSITOR_DIRTY_TIME as f64;

        beat_delta
    }

    // ─── Core tick ───
    //
    // Orchestration order matches Unity PlaybackController.Update() (lines 1055-1218)
    // exactly, so we never need to revisit this structure. Individual method
    // implementations may evolve (especially video-related), but the call sites
    // and their ordering are final.

    /// Advance playback by one frame. Returns compositor instructions.
    /// Must not be called re-entrantly.
    ///
    /// Port of C# PlaybackController.Update() orchestration (lines 1055-1218).
    /// The engine owns the full orchestration that Unity splits across
    /// PlaybackController (MonoBehaviour) and PlaybackEngine (plain class).
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

        // ── Phase 1: External beat derivation (stub) ──
        // Port of C# PlaybackController.Update lines 1064-1096.
        // When Link/MidiClock sync controllers are wired (GAP-PLAY-9),
        // external beat injection will go here:
        //   - Link authority: engine.set_beat(link.beat - link_beat_offset)
        //   - MidiClock authority: engine.set_beat((sixteenths + tick/6) / 4)
        //   - Otherwise: beat derived from time (already happens in advance_time)

        // ── Phase 2: Tempo recording/resolution (stub) ──
        // Port of C# PlaybackController.Update lines 1098-1099.
        // UpdateRecordingSessionState(authority) → TempoRecorder (not yet ported)
        // ApplyResolvedTempo(authority) → TryResolveExternalTempo (not yet ported)
        // When wired, this will pull live BPM from Link/MidiClock and either
        // record tempo automation or update the global BPM.

        // ── Phase 3: Shared pre-branch (all states) ──
        // Port of C# PlaybackController.Update lines 1102-1112.
        self.sync_project_bpm_from_current_beat();
        self.process_pending_pauses(ctx.realtime_now);
        self.check_preparing_clips();

        // ── Phase 4: Branch on playback state ──
        let result = if self.current_state == PlaybackState::Playing {
            self.tick_playing(ctx)
        } else {
            self.tick_non_playing(ctx)
        };

        self.is_ticking = false;
        result
    }

    /// Playing-state tick. Matches C# PlaybackController.Update lines 1135-1218.
    fn tick_playing(&mut self, ctx: TickContext) -> TickResult {
        // 1. Clear deferred sync flag — SyncClipsToTime below handles it.
        //    Port of C# line 1138.
        self.consume_sync_dirty();

        // 2. Advance time (unless external sync source is the clock authority).
        //    Port of C# lines 1141-1150.
        if !self.external_time_sync {
            let frame_delta = if self.is_export_mode && ctx.export_fixed_dt > 0.0 {
                ctx.export_fixed_dt
            } else {
                ctx.dt_seconds
            };
            self.advance_time(frame_delta * self.playback_speed as f64);
            self.sync_project_bpm_from_current_beat();

            // Fire on_time_changed callback. Port of C# line 1149.
            if let Some(ref cb) = self.on_time_changed {
                cb(self.current_time);
            }
        }

        // 3. Activate pending live MIDI launches whose target tick has arrived.
        //    Port of C# line 1152: engine.LiveClipMgr.ActivateDuePendingLiveLaunches().
        let live_activated = if let Some(ref mut mgr) = self.live_clip_manager {
            let now_tick = self.last_frame_count; // absolute tick from frame count
            mgr.activate_due_pending_launches_at_tick(now_tick)
        } else {
            false
        };
        if live_activated {
            self.sync_clips_dirty = true;
        }

        // 4. Sync clips to current time (start/stop as needed).
        //    Port of C# lines 1155-1158.
        self.sync_clips_to_time();

        // 5. Keep active video playback rates aligned with current tempo/beat.
        //    Port of C# line 1161.
        self.update_active_clip_playback_rates();

        // 6. Per-frame boundary enforcement for custom loop duration clips.
        //    Must run BEFORE drift correction, which skips all looping clips.
        //    Port of C# line 1166.
        self.check_custom_loop_boundaries();

        // 7. Evaluate modulation pipeline (LFO drivers + ADSR envelopes).
        //    Port of C# DriverController.Update() [ExecutionOrder 50, after PlaybackController].
        let modulation_dirty = if let Some(project) = &mut self.project {
            crate::modulation::evaluate_modulation(project, self.current_beat)
        } else {
            false
        };
        if modulation_dirty {
            self.mark_compositor_dirty(ctx.realtime_now);
        }

        // 8. Drift correction BEFORE compositor so any clip stops are reflected immediately.
        //    Skipped during export — re-seeking every 2s causes visible stutters.
        //    Port of C# lines 1175-1183.
        if !self.is_export_mode
            && self.current_time - self.last_sync_time >= self.video_sync_interval
        {
            self.correct_video_drift();
            self.last_sync_time = self.current_time;
        }

        // 9. Filter ready clips for compositor (full filtering with pre_render + recently-started).
        //    Replaces the simpler build_ready_clips_list with Unity's FilterReadyClips.
        //    Port of C# UpdateCompositor → engine.FilterReadyClips (lines 1432-1458).
        let ready = self.filter_ready_clips(ctx.pre_render_dt);

        let compositor_dirty = !ready.is_empty()
            || ctx.realtime_now < self.compositor_dirty_deadline;
        let should_clear = ready.is_empty() && !self.has_pending_clip_state();

        // 10. Lookahead prewarm — engine computes candidates, caller executes pool pre-warm.
        //     Port of C# line 1217: UpdateLookaheadPrewarm(force: false).
        //     Candidates are returned in TickResult for the caller (app.rs) to act on.
        let _prewarm = self.compute_prewarm_candidates(false);

        TickResult {
            ready_clips: ready,
            compositor_dirty,
            should_clear_compositor: should_clear,
            should_clear_feedback_buffer: false,
        }
    }

    /// Non-playing (paused/stopped) tick. Matches C# PlaybackController.Update lines 1114-1133.
    fn tick_non_playing(&mut self, ctx: TickContext) -> TickResult {
        // 1. Flush deferred sync from MIDI events.
        //    Port of C# lines 1117-1120.
        if self.consume_sync_dirty() {
            self.sync_clips_to_time();
        }

        // 2. Keep active clip playback rates aligned.
        //    Port of C# line 1122.
        self.update_active_clip_playback_rates();

        // 3. Evaluate modulation pipeline even when stopped (for scrub preview / inspector).
        //    Port of C# DriverController — runs in all states.
        if let Some(project) = &mut self.project {
            if crate::modulation::evaluate_modulation(project, self.current_beat) {
                self.mark_compositor_dirty(ctx.realtime_now);
            }
        }

        // 4. Filter ready clips for compositor.
        //    Port of C# UpdateCompositor (lines 1126-1132).
        //    Only runs while compositor dirty deadline is active or generators are running.
        let has_generators = self.active_clip_renderers.iter().any(|(_, &idx)| {
            !self.renderers[idx].needs_prepare_phase()
        });
        let compositor_dirty = ctx.realtime_now < self.compositor_dirty_deadline || has_generators;

        let ready = if compositor_dirty {
            self.filter_ready_clips(ctx.pre_render_dt)
        } else {
            Vec::new()
        };

        TickResult {
            ready_clips: ready,
            compositor_dirty,
            should_clear_compositor: !compositor_dirty
                && self.active_clip_renderers.is_empty()
                && !self.has_pending_clip_state(),
            should_clear_feedback_buffer: false,
        }
    }

    /// Build the ready_clips_list from currently active clips.
    /// Superseded by filter_ready_clips() in the tick loop, but kept for
    /// potential use in tests or simple queries.
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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

    pub fn process_pending_pauses(&mut self, realtime_now: f64) {
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

    // ─── Prewarm ───

    /// Invalidate the prewarm cache so the next tick rebuilds it.
    /// Port of C# PlaybackEngine.InvalidatePrewarm (line 418).
    pub fn invalidate_prewarm(&mut self) {
        self.next_prewarm_at = 0.0;
        self.last_prewarm_ids.clear();
    }

    // ─── Clip loop state ───

    /// Update a clip's looping state in the engine's tracking sets and notify its renderer.
    /// Port of C# PlaybackEngine.SyncClipLoopState (lines 1111-1138).
    pub fn sync_clip_loop_state(&mut self, clip: &TimelineClip) {
        let clip_id = &clip.id;

        if clip.is_looping {
            self.looping_clip_ids.insert(clip_id.clone());
        } else {
            self.looping_clip_ids.remove(clip_id);
        }

        if let Some(&renderer_idx) = self.active_clip_renderers.get(clip_id) {
            self.renderers[renderer_idx].set_clip_looping(clip_id, clip.is_looping);

            // Apply playback rate after loop change
            let rate = self.compute_clip_playback_rate(clip);
            self.renderers[renderer_idx].set_clip_playback_rate(clip_id, rate);

            // Seek to correct loop position if looping enabled
            if clip.is_looping && clip.loop_duration_beats > 0.0 {
                let bpm = self.project.as_ref().map(|p| p.settings.bpm).unwrap_or(120.0);
                let spb = 60.0 / bpm.max(20.0);
                let clip_start_time = clip.start_beat * spb;
                let loop_dur_seconds = clip.loop_duration_beats * spb;
                let media_length = self.renderers[renderer_idx].get_clip_media_length(clip_id);
                let video_time = crate::video_time::compute_video_time(
                    self.current_time,
                    clip_start_time,
                    clip.in_point,
                    clip.is_looping,
                    loop_dur_seconds,
                    media_length,
                    rate,
                );
                self.renderers[renderer_idx].seek_clip(clip_id, video_time);
            }
        }
    }

    /// Compute clip playback rate matching Unity's ApplyClipPlaybackRate.
    fn compute_clip_playback_rate(&self, clip: &TimelineClip) -> f32 {
        if clip.recorded_bpm > 0.0 {
            let current_bpm = self.project.as_ref().map(|p| p.settings.bpm).unwrap_or(120.0);
            (current_bpm / clip.recorded_bpm).clamp(MIN_CLIP_PLAYBACK_RATE, MAX_CLIP_PLAYBACK_RATE)
        } else {
            1.0
        }
    }

    // ─── Pending pause management ───

    /// Clear all pending pauses.
    /// Port of C# PlaybackEngine.ClearPendingPauses (line 1171).
    pub fn clear_pending_pauses(&mut self) {
        self.pending_pauses.clear();
        self.to_pause_list.clear();
    }

    // ─── Active clip queries ───

    /// Return timeline active clips at the current beat.
    /// Port of C# PlaybackEngine.GetTimelineActiveClipsAtCurrentBeat (lines 1031-1056).
    pub fn get_timeline_active_clips_at_current_beat(&mut self) -> &[TimelineClip] {
        self.query_active_timeline_clips();
        &self.timeline_active_scratch
    }

    // ─── Sync ───

    /// Re-synchronize active clips to current playback position.
    /// Called by play() and seek_to() for immediate state consistency.
    /// The heart of deterministic playback — idempotent.
    pub fn sync_clips_to_time(&mut self) {
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

        let live_slots = if let Some(mgr) = &self.live_clip_manager {
            mgr.live_slots_list()
        } else {
            &[]
        };

        let sync_result = self.scheduler.compute_sync(
            self.current_time,
            self.current_beat,
            &self.timeline_active_scratch,
            live_slots,
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

    /// Process MIDI input events for the current frame.
    /// Extracts project and live_clip_manager from self to avoid borrow conflict,
    /// then calls MidiInputController::update with self as the LiveClipHost.
    pub fn tick_midi_input(
        &mut self,
        midi_input: &mut crate::midi_input::MidiInputController,
        clip_launcher: &mut crate::clip_launcher::ClipLauncher,
        realtime_now: f64,
    ) {
        // Get clock authority without borrowing project mutably.
        let clock_authority = self.project.as_ref()
            .map(|p| p.settings.clock_authority)
            .unwrap_or(manifold_core::types::ClockAuthority::Internal);

        if self.project.is_none() || self.live_clip_manager.is_none() {
            return;
        }

        // Safety: We need &mut Project and &mut LiveClipManager while also passing
        // &mut self as LiveClipHost. Rust can't express this borrow split through
        // the public API, so we use pointer-level split borrow here.
        // This is safe because the LiveClipHost methods on PlaybackEngine that take
        // &mut self only mutate fields that are NOT project or live_clip_manager
        // (stop_clip mutates active_clip_renderers etc., mark_sync_dirty sets a bool,
        // mark_compositor_dirty sets a deadline, invalidate_lookahead_prewarm resets a timer).
        let project = self.project.as_mut().unwrap() as *mut manifold_core::project::Project;
        let live_clip_manager = self.live_clip_manager.as_mut().unwrap() as *mut crate::live_clip_manager::LiveClipManager;

        // SAFETY: project and live_clip_manager are distinct fields from those
        // mutated by the LiveClipHost trait methods called inside update().
        let project_ref = unsafe { &mut *project };
        let lcm_ref = unsafe { &mut *live_clip_manager };

        midi_input.update(
            clock_authority,
            project_ref,
            clip_launcher,
            lcm_ref,
            self,
            realtime_now,
        );
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

    // ─── Clip time/rate methods ───
    // Port of C# PlaybackEngine lines 1449-1588.

    /// Get clip start time in timeline seconds.
    /// Port of C# PlaybackEngine.GetClipStartTimeSeconds (lines 1449-1453).
    pub fn get_clip_start_time_seconds(&mut self, clip: &TimelineClip) -> f32 {
        self.beat_to_timeline_time(clip.start_beat)
    }

    /// Get clip end time in timeline seconds.
    /// Port of C# PlaybackEngine.GetClipEndTimeSeconds (lines 1456-1460).
    pub fn get_clip_end_time_seconds(&mut self, clip: &TimelineClip) -> f32 {
        self.beat_to_timeline_time(clip.end_beat())
    }

    /// Get clip duration in timeline seconds.
    /// Port of C# PlaybackEngine.GetClipDurationSeconds (lines 1463-1468).
    pub fn get_clip_duration_seconds(&mut self, clip: &TimelineClip) -> f32 {
        let duration = self.get_clip_end_time_seconds(clip) - self.get_clip_start_time_seconds(clip);
        duration.max(0.0)
    }

    /// Get clip loop duration in timeline seconds.
    /// Port of C# PlaybackEngine.GetClipLoopDurationSeconds (lines 1471-1477).
    pub fn get_clip_loop_duration_seconds(&mut self, clip: &TimelineClip) -> f32 {
        if clip.loop_duration_beats <= 0.0 { return 0.0; }
        let loop_end = self.beat_to_timeline_time(clip.start_beat + clip.loop_duration_beats);
        let loop_start = self.get_clip_start_time_seconds(clip);
        (loop_end - loop_start).max(0.0)
    }

    /// Resolve the effective recorded BPM for a clip.
    /// Checks per-clip BPM first, then project recording provenance, else 0.
    /// Port of C# PlaybackEngine.ResolveClipRecordedBpm (lines 1480-1492).
    pub fn resolve_clip_recorded_bpm(&self, clip: &TimelineClip) -> f32 {
        if clip.recorded_bpm > 0.0 {
            return clip.recorded_bpm;
        }
        if let Some(project) = &self.project {
            if project.recording_provenance.has_recorded_project_bpm {
                let bpm = project.recording_provenance.recorded_project_bpm;
                return bpm.clamp(20.0, 300.0);
            }
        }
        0.0
    }

    /// Get playback rate for BPM time-stretching.
    /// Returns 1.0 for generators or clips without recorded BPM.
    /// Port of C# PlaybackEngine.GetClipPlaybackRate (lines 1495-1505).
    pub fn get_clip_playback_rate(&mut self, clip: &TimelineClip) -> f32 {
        if clip.is_generator() { return 1.0; }

        let recorded_bpm = self.resolve_clip_recorded_bpm(clip);
        if recorded_bpm <= 0.0 { return 1.0; }

        let timeline_bpm = self.get_bpm_at_beat(self.current_beat).clamp(20.0, 300.0);
        let rate = timeline_bpm / recorded_bpm;
        rate.clamp(MIN_CLIP_PLAYBACK_RATE, MAX_CLIP_PLAYBACK_RATE)
    }

    /// Try to get the recorded seconds-per-beat for a clip.
    /// Port of C# PlaybackEngine.TryGetClipRecordedSpb (lines 1508-1516).
    pub fn try_get_clip_recorded_spb(&self, clip: &TimelineClip) -> Option<f32> {
        let recorded_bpm = self.resolve_clip_recorded_bpm(clip);
        if recorded_bpm <= 0.0 { return None; }
        let spb = TempoMapConverter::seconds_per_beat_from_bpm(recorded_bpm);
        if spb > 0.0 { Some(spb) } else { None }
    }

    /// Get elapsed source-time seconds for a clip at the current playhead.
    /// Port of C# PlaybackEngine.GetClipSourceElapsedSeconds (lines 1519-1532).
    pub fn get_clip_source_elapsed_seconds(&mut self, clip: &TimelineClip) -> f32 {
        if let Some(recorded_spb) = self.try_get_clip_recorded_spb(clip) {
            let elapsed_beats = (self.current_beat - clip.start_beat).max(0.0);
            return elapsed_beats * recorded_spb;
        }
        let clip_start_time = self.get_clip_start_time_seconds(clip);
        let clip_local_time = (self.current_time - clip_start_time).max(0.0);
        clip_local_time * self.get_clip_playback_rate(clip)
    }

    /// Get total source duration in seconds for a clip.
    /// Port of C# PlaybackEngine.GetClipSourceDurationSeconds (lines 1535-1543).
    pub fn get_clip_source_duration_seconds(&mut self, clip: &TimelineClip) -> f32 {
        if let Some(recorded_spb) = self.try_get_clip_recorded_spb(clip) {
            return (clip.duration_beats * recorded_spb).max(0.0);
        }
        (self.get_clip_duration_seconds(clip) * self.get_clip_playback_rate(clip)).max(0.0)
    }

    /// Get source-time loop duration in seconds for a clip.
    /// Port of C# PlaybackEngine.GetClipSourceLoopDurationSeconds (lines 1546-1554).
    pub fn get_clip_source_loop_duration_seconds(&mut self, clip: &TimelineClip) -> f32 {
        if clip.loop_duration_beats <= 0.0 { return 0.0; }
        if let Some(recorded_spb) = self.try_get_clip_recorded_spb(clip) {
            return (clip.loop_duration_beats * recorded_spb).max(0.0);
        }
        (self.get_clip_loop_duration_seconds(clip) * self.get_clip_playback_rate(clip)).max(0.0)
    }

    /// Compute video time for a clip (beat-domain, with looping).
    /// Uses source-elapsed and in-point. Port of C# PlaybackEngine.ComputeVideoTime
    /// (lines 1561-1581).
    pub fn compute_video_time(&mut self, clip: &TimelineClip, clip_id: &str) -> f32 {
        let source_elapsed = self.get_clip_source_elapsed_seconds(clip);

        // Get media length from renderer if looping
        let media_length = if clip.is_looping {
            self.get_clip_media_length(clip_id)
        } else {
            0.0
        };

        if clip.is_looping && media_length > 0.01 {
            let source_available = (media_length - clip.in_point).max(0.0);
            let loop_len_sec = if clip.loop_duration_beats > 0.0 {
                self.get_clip_source_loop_duration_seconds(clip).min(source_available)
            } else {
                media_length
            };

            if loop_len_sec > 0.01 {
                return clip.in_point + (source_elapsed % loop_len_sec);
            }
        }

        clip.in_point + source_elapsed
    }

    /// Apply the playback rate to a renderer for a clip.
    /// Port of C# PlaybackEngine.ApplyClipPlaybackRate (lines 1584-1588).
    pub fn apply_clip_playback_rate(&mut self, clip_id: &str, clip: &TimelineClip) {
        let rate = self.get_clip_playback_rate(clip);
        if let Some(&idx) = self.active_clip_renderers.get(clip_id) {
            self.renderers[idx].set_clip_playback_rate(clip_id, rate);
        }
    }

    /// Get media length from the active renderer for a clip.
    fn get_clip_media_length(&self, clip_id: &str) -> f32 {
        if let Some(&idx) = self.active_clip_renderers.get(clip_id) {
            self.renderers[idx].get_clip_media_length(clip_id)
        } else {
            0.0
        }
    }

    // ─── Clip maintenance methods ───

    /// Poll preparing clips for readiness. When ready: seek, resume, track as
    /// recently-started. Port of C# PlaybackEngine.CheckPreparingClips (lines 758-815).
    pub fn check_preparing_clips(&mut self) {
        if self.preparing_clips.is_empty() { return; }

        self.became_ready_list.clear();

        let preparing_list: Vec<String> = self.preparing_clips.iter().cloned().collect();
        for clip_id in &preparing_list {
            let renderer_idx = match self.active_clip_renderers.get(clip_id.as_str()) {
                Some(&idx) => idx,
                None => {
                    // Clip was stopped while preparing — clean up
                    self.became_ready_list.push(clip_id.clone());
                    continue;
                }
            };

            if !self.renderers[renderer_idx].is_clip_ready(clip_id) {
                continue;
            }

            // Clip is prepared!
            self.became_ready_list.push(clip_id.clone());

            let clip = match self.find_timeline_clip(clip_id).cloned() {
                Some(c) => c,
                None => continue,
            };

            // Apply rate and seek
            let rate = self.get_clip_playback_rate(&clip);
            self.renderers[renderer_idx].set_clip_playback_rate(clip_id, rate);
            let video_time = self.compute_video_time(&clip, clip_id);
            self.renderers[renderer_idx].seek_clip(clip_id, video_time);

            if self.looping_clip_ids.contains(clip_id) {
                self.renderers[renderer_idx].set_clip_looping(clip_id, true);
            }

            self.renderers[renderer_idx].resume_clip(clip_id);

            // Exclude from compositor until first frame decodes
            self.recently_started_times.insert(clip_id.clone(), self.last_realtime_now);

            if self.current_state != PlaybackState::Playing
                && self.renderers[renderer_idx].needs_pending_pause()
            {
                let deadline = self.last_realtime_now + PENDING_PAUSE_DELAY as f64;
                self.pending_pauses.insert(clip_id.clone(), deadline);
            }

            self.compositor_dirty_deadline =
                self.last_realtime_now + PENDING_PAUSE_DELAY as f64 + COMPOSITOR_DIRTY_TIME as f64;
        }

        for id in &self.became_ready_list {
            self.preparing_clips.remove(id.as_str());
        }
    }

    /// Enforce custom loop boundaries for looping clips with custom loop durations.
    /// Port of C# PlaybackEngine.CheckCustomLoopBoundaries (lines 820-854).
    pub fn check_custom_loop_boundaries(&mut self) {
        let looping_list: Vec<String> = self.looping_clip_ids.iter().cloned().collect();
        for clip_id in &looping_list {
            let renderer_idx = match self.active_clip_renderers.get(clip_id.as_str()) {
                Some(&idx) => idx,
                None => continue,
            };

            if !self.renderers[renderer_idx].needs_prepare_phase() { continue; }
            if self.preparing_clips.contains(clip_id) { continue; }
            if !self.renderers[renderer_idx].is_clip_ready(clip_id) { continue; }

            let clip = match self.find_timeline_clip(clip_id).cloned() {
                Some(c) => c,
                None => continue,
            };
            if clip.loop_duration_beats <= 0.0 { continue; }

            let media_length = self.renderers[renderer_idx].get_clip_media_length(clip_id);
            let source_available = (media_length - clip.in_point).max(0.0);
            let loop_len_sec = self.get_clip_source_loop_duration_seconds(&clip).min(source_available);

            if loop_len_sec < 0.01 { continue; }

            let boundary = clip.in_point + loop_len_sec;

            if self.renderers[renderer_idx].get_clip_playback_time(clip_id) >= boundary {
                self.renderers[renderer_idx].pause_clip(clip_id);
                self.renderers[renderer_idx].seek_clip(clip_id, clip.in_point);
                let rate = self.get_clip_playback_rate(&clip);
                self.renderers[renderer_idx].set_clip_playback_rate(clip_id, rate);
                self.renderers[renderer_idx].resume_clip(clip_id);
            }
        }
    }

    /// Correct video drift: re-seek players that have drifted from expected position,
    /// stop clips past their out-point, restart stopped players.
    /// Port of C# PlaybackEngine.CorrectVideoDrift (lines 859-947).
    pub fn correct_video_drift(&mut self) {
        if self.project.is_none() { return; }

        self.clips_to_stop_drift.clear();

        let active_list: Vec<(String, usize)> = self.active_clip_renderers
            .iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect();

        for (clip_id, renderer_idx) in &active_list {
            if !self.renderers[*renderer_idx].needs_drift_correction() { continue; }
            if self.preparing_clips.contains(clip_id) { continue; }
            if !self.renderers[*renderer_idx].is_clip_ready(clip_id) { continue; }

            let clip = match self.find_timeline_clip(clip_id).cloned() {
                Some(c) => c,
                None => continue,
            };

            let rate = self.get_clip_playback_rate(&clip);
            self.renderers[*renderer_idx].set_clip_playback_rate(clip_id, rate);

            // Looping clips managed by native looping — skip drift correction
            if self.looping_clip_ids.contains(clip_id) { continue; }

            let expected_video_time = clip.in_point + self.get_clip_source_elapsed_seconds(&clip);
            let out_point = clip.in_point + self.get_clip_source_duration_seconds(&clip);

            let playback_time = self.renderers[*renderer_idx].get_clip_playback_time(clip_id);
            let media_length = self.renderers[*renderer_idx].get_clip_media_length(clip_id);

            let is_live_slot = self.live_clip_manager.as_ref()
                .is_some_and(|mgr| mgr.is_live_slot_clip(clip_id));

            // Out-point enforcement
            if !is_live_slot && playback_time >= out_point {
                self.clips_to_stop_drift.push(clip_id.clone());
                continue;
            }

            // Video reached natural end of file
            if media_length > 0.0 && playback_time >= media_length - 0.1 {
                if is_live_slot {
                    self.renderers[*renderer_idx].seek_clip(clip_id, clip.in_point);
                    self.renderers[*renderer_idx].set_clip_playback_rate(clip_id, rate);
                    if self.current_state == PlaybackState::Playing
                        && !self.renderers[*renderer_idx].is_clip_playing(clip_id)
                    {
                        self.renderers[*renderer_idx].resume_clip(clip_id);
                    }
                } else if self.renderers[*renderer_idx].is_clip_playing(clip_id) {
                    self.renderers[*renderer_idx].pause_clip(clip_id);
                }
                continue;
            }

            // Live slots: avoid seek-based drift correction
            if is_live_slot {
                if !self.renderers[*renderer_idx].is_clip_playing(clip_id)
                    && self.current_state == PlaybackState::Playing
                {
                    self.renderers[*renderer_idx].resume_clip(clip_id);
                }
                continue;
            }

            // Player stopped unexpectedly — restart it
            let reached_end = media_length > 0.0 && expected_video_time >= media_length - 0.1;
            if !self.renderers[*renderer_idx].is_clip_playing(clip_id)
                && self.current_state == PlaybackState::Playing
                && !reached_end
            {
                self.renderers[*renderer_idx].seek_clip(clip_id, expected_video_time);
                self.renderers[*renderer_idx].set_clip_playback_rate(clip_id, rate);
                self.renderers[*renderer_idx].resume_clip(clip_id);
                if let Some(ref log_warn) = self.log_warning {
                    log_warn(&format!("[PlaybackEngine] Restarted stopped player: {clip_id}"));
                }
                continue;
            }

            // Drift correction
            let drift = (playback_time - expected_video_time).abs();
            if drift > 0.1 {
                self.renderers[*renderer_idx].seek_clip(clip_id, expected_video_time);
                self.renderers[*renderer_idx].set_clip_playback_rate(clip_id, rate);
                self.drift_correction_count += 1;
                if let Some(ref log_warn) = self.log_warning {
                    log_warn(&format!("[PlaybackEngine] Drift correction: {clip_id} ({drift:.3}s)"));
                }
            }
        }

        // Stop clips that exceeded their out-point (deferred to avoid borrow conflict)
        let to_stop: Vec<String> = self.clips_to_stop_drift.drain(..).collect();
        for clip_id in &to_stop {
            self.stop_clip(clip_id);
        }
    }

    /// Re-apply playback rates to all active clips.
    /// Port of C# PlaybackEngine.UpdateActiveClipPlaybackRates (lines 952-962).
    pub fn update_active_clip_playback_rates(&mut self) {
        let active_list: Vec<(String, usize)> = self.active_clip_renderers
            .iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect();

        for (clip_id, renderer_idx) in &active_list {
            let clip = match self.find_timeline_clip(clip_id).cloned() {
                Some(c) => c,
                None => continue,
            };
            let rate = self.get_clip_playback_rate(&clip);
            self.renderers[*renderer_idx].set_clip_playback_rate(clip_id, rate);
        }
    }

    /// Re-seek all active seekable clips to current playhead position.
    /// Port of C# PlaybackEngine.SeekActiveClips (lines 967-987).
    pub fn seek_active_clips(&mut self) {
        let active_list: Vec<(String, usize)> = self.active_clip_renderers
            .iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect();

        for (clip_id, renderer_idx) in &active_list {
            if !self.renderers[*renderer_idx].needs_prepare_phase() { continue; }
            if self.preparing_clips.contains(clip_id.as_str()) { continue; }
            if !self.renderers[*renderer_idx].is_clip_ready(clip_id) { continue; }

            let clip = match self.find_timeline_clip(clip_id).cloned() {
                Some(c) => c,
                None => continue,
            };

            let rate = self.get_clip_playback_rate(&clip);
            self.renderers[*renderer_idx].set_clip_playback_rate(clip_id, rate);
            let video_time = self.compute_video_time(&clip, clip_id);
            self.renderers[*renderer_idx].seek_clip(clip_id, video_time);
        }

        self.compositor_dirty_deadline = self.last_realtime_now + COMPOSITOR_DIRTY_TIME as f64;
    }

    /// Check if a recently-started clip should be excluded from compositor output.
    /// Port of C# PlaybackEngine.ShouldExcludeRecentlyStarted (lines 1090-1105).
    fn should_exclude_recently_started(
        &self,
        clip_id: &str,
        clip_end_time_seconds: f32,
        is_live_clip: bool,
    ) -> bool {
        let start_time = match self.recently_started_times.get(clip_id) {
            Some(&t) => t,
            None => return false,
        };

        let mut gate_time = if is_live_clip {
            LIVE_RECENTLY_STARTED_TIME
        } else {
            RECENTLY_STARTED_TIME
        };
        let remaining = clip_end_time_seconds - self.current_time;
        if remaining > 0.0 {
            gate_time = gate_time.min(remaining * 0.4);
        }

        (self.last_realtime_now - start_time) < gate_time as f64
    }

    /// Filter active clips to only those ready for compositing.
    /// Applies recently-started gate for video clips.
    /// Port of C# PlaybackEngine.FilterReadyClips (lines 1193-1239).
    pub fn filter_ready_clips(&mut self, pre_render_dt: f32) -> Vec<TimelineClip> {
        // Resolve should-be-active clips (timeline + live slots)
        self.compositor_fallback_clips.clear();
        self.query_active_timeline_clips();
        self.compositor_fallback_clips.extend(self.timeline_active_scratch.iter().cloned());
        if let Some(mgr) = &self.live_clip_manager {
            for (_, clip) in mgr.live_slots_list() {
                self.compositor_fallback_clips.push(clip.clone());
            }
        }

        // Pre-render all renderers (generators blit shaders, video is no-op)
        for renderer in &mut self.renderers {
            renderer.pre_render(self.current_time, self.current_beat, pre_render_dt);
        }

        // Filter to ready clips (index-based to avoid borrow conflict)
        self.ready_clips_list.clear();
        for i in 0..self.compositor_fallback_clips.len() {
            let clip = &self.compositor_fallback_clips[i];
            let clip_id = clip.id.clone();
            let renderer_idx = match self.active_clip_renderers.get(clip_id.as_str()) {
                Some(&idx) => idx,
                None => continue,
            };
            if !self.renderers[renderer_idx].is_clip_ready(&clip_id) { continue; }

            // Skip clips whose RenderTexture hasn't had time to decode (video-specific)
            if self.renderers[renderer_idx].needs_prepare_phase() {
                let is_live_clip = self.live_clip_manager.as_ref()
                    .is_some_and(|mgr| mgr.is_live_slot_clip(&clip_id));
                // Inline beat_to_timeline_time to avoid &mut self borrow
                let clip_end_time = if let Some(project) = &self.project {
                    TempoMapConverter::beat_to_seconds_immut(
                        &project.tempo_map,
                        clip.end_beat(),
                        project.settings.bpm,
                    )
                } else {
                    clip.end_beat() * 0.5
                };
                if self.should_exclude_recently_started(&clip_id, clip_end_time, is_live_clip) {
                    continue;
                }
            }

            self.ready_clips_list.push(self.compositor_fallback_clips[i].clone());
        }

        // Sort by layer index descending (back to front for compositing)
        self.ready_clips_list.sort_by(|a, b| b.layer_index.cmp(&a.layer_index));

        // Clear expired recently-started entries that passed the gate
        let last_rt = self.last_realtime_now;
        self.recently_started_times.retain(|_id, &mut start_time| {
            last_rt - start_time < RECENTLY_STARTED_TIME as f64
        });

        self.ready_clips_list.clone()
    }

    /// Compute pre-warm candidates: clips near the playhead that should have decoders started.
    /// Port of C# PlaybackEngine.ComputePrewarmCandidates (lines 1251-1330).
    pub fn compute_prewarm_candidates(&mut self, force: bool) -> Option<HashMap<String, crate::video_time::PrewarmCandidate>> {
        if self.project.as_ref().map_or(true, |p| p.video_library.clips.is_empty()) {
            return None;
        }

        if !force && self.last_realtime_now < self.next_prewarm_at { return None; }

        let in_live_burst = if let Some(mgr) = &self.live_clip_manager {
            (self.last_realtime_now - mgr.last_live_trigger_at()) <= LIVE_PREWARM_BURST_TIME as f64
        } else {
            false
        };
        self.next_prewarm_at = self.last_realtime_now
            + if in_live_burst { LIVE_PREWARM_INTERVAL } else { LOOKAHEAD_PREWARM_INTERVAL } as f64;

        let window_start = self.current_time - LOOKAHEAD_PREWARM_BEHIND_TIME;
        let window_end = self.current_time + LOOKAHEAD_PREWARM_AHEAD_TIME;

        // Collect candidate clips (use immutable beat_to_seconds to avoid &mut self borrow)
        self.prewarm_candidates.clear();
        if let Some(project) = &self.project {
            let any_solo = project.timeline.layers.iter().any(|l| l.is_solo);
            let fallback_bpm = project.settings.bpm;

            for layer in &project.timeline.layers {
                if layer.is_muted { continue; }
                if any_solo && !layer.is_solo { continue; }

                for clip in &layer.clips {
                    if clip.is_generator() || clip.is_muted { continue; }
                    if clip.video_clip_id.is_empty() { continue; }

                    let clip_start = TempoMapConverter::beat_to_seconds_immut(
                        &project.tempo_map, clip.start_beat, fallback_bpm);
                    let clip_end = TempoMapConverter::beat_to_seconds_immut(
                        &project.tempo_map, clip.end_beat(), fallback_bpm);
                    if clip_end < window_start { continue; }
                    if clip_start > window_end { continue; }
                    self.prewarm_candidates.push(clip.clone());
                }
            }
        }

        self.prewarm_candidates.sort_by(|a, b| a.start_beat.partial_cmp(&b.start_beat).unwrap_or(std::cmp::Ordering::Equal));

        // Build prewarm set from candidates
        let mut prewarm_set: HashMap<String, crate::video_time::PrewarmCandidate> = HashMap::new();
        if let Some(project) = &self.project {
            for clip in &self.prewarm_candidates {
                if prewarm_set.len() >= LOOKAHEAD_PREWARM_MAX_UNIQUE_CLIPS { break; }
                if prewarm_set.contains_key(&clip.video_clip_id) { continue; }

                if let Some(vc) = project.video_library.find_clip_by_id(&clip.video_clip_id) {
                    prewarm_set.insert(clip.video_clip_id.clone(), crate::video_time::PrewarmCandidate {
                        video_clip_id: vc.id.clone(),
                        file_path: vc.file_path.clone(),
                    });
                }
            }
        }

        // Change detection
        let changed = prewarm_set.len() != self.last_prewarm_ids.len()
            || prewarm_set.keys().any(|k| !self.last_prewarm_ids.contains(k));

        if !changed { return None; }

        self.last_prewarm_ids.clear();
        self.last_prewarm_ids.extend(prewarm_set.keys().cloned());

        Some(prewarm_set)
    }
}

// ─── LiveClipHost impl for PlaybackEngine ───────────────────────────────────

use crate::live_clip_manager::LiveClipHost;

/// PlaybackEngine implements LiveClipHost so it can be passed directly to
/// ClipLauncher / LiveClipManager without a separate adapter type.
/// Port of C# PlaybackController implementing ILiveClipHost.
impl LiveClipHost for PlaybackEngine {
    fn current_beat(&self) -> f32 { self.current_beat }
    fn current_time(&self) -> f32 { self.current_time }
    fn is_recording(&self) -> bool { self.is_recording }
    fn is_playing(&self) -> bool { self.current_state == PlaybackState::Playing }
    fn show_debug_logs(&self) -> bool { self.show_debug_logs }

    /// BPM at the given beat. Checks live external tempo first.
    fn get_bpm_at_beat(&self, beat: f32) -> f32 {
        if let Some((live_bpm, _)) = self.try_get_live_external_tempo() {
            return live_bpm;
        }
        if let Some(project) = &self.project {
            // Immutable scan (tempo map is kept sorted by ensure_sorted on mutation).
            let fallback = project.settings.bpm;
            let points = project.tempo_map.clone_points();
            if points.is_empty() {
                return fallback.clamp(20.0, 300.0);
            }
            let mut bpm = points[0].bpm;
            for point in &points {
                if point.beat <= beat {
                    bpm = point.bpm;
                } else {
                    break;
                }
            }
            bpm.clamp(20.0, 300.0)
        } else {
            120.0
        }
    }

    fn get_tempo_source_at_beat(&self, _beat: f32) -> TempoPointSource {
        // Live external tempo overrides the source.
        if let Some((_, source)) = self.try_get_live_external_tempo() {
            return source;
        }
        TempoPointSource::Unknown
    }

    fn get_beat_snapped_beat(&self) -> f32 {
        if let Some(ref resolver) = self.beat_snapped_beat_resolver {
            resolver()
        } else {
            self.current_beat
        }
    }

    fn get_current_absolute_tick(&self) -> i32 {
        if let Some(ref resolver) = self.absolute_tick_resolver {
            resolver()
        } else {
            self.last_frame_count
        }
    }

    fn stop_clip(&mut self, clip_id: &str) {
        PlaybackEngine::stop_clip(self, clip_id);
    }

    fn mark_sync_dirty(&mut self) {
        PlaybackEngine::mark_sync_dirty(self);
    }

    fn mark_compositor_dirty(&mut self) {
        let now = self.last_realtime_now;
        PlaybackEngine::mark_compositor_dirty(self, now);
    }

    fn invalidate_lookahead_prewarm(&mut self) {
        self.next_prewarm_at = 0.0;
    }

    fn register_clip_lookup(&mut self, _clip_id: &str, _clip: &manifold_core::clip::TimelineClip) {
        // PlaybackEngine looks up clips via the timeline and live_clip_manager.
        // No separate lookup table is needed — the engine's find_timeline_clip
        // already searches both the timeline and live slots.
    }

    fn record_command(&mut self, cmd: Box<dyn manifold_editing::command::Command>) {
        if let Some(ref delegate) = self.record_command_delegate {
            delegate(cmd);
        }
    }

    fn beat_to_timeline_time(&self, beat: f32) -> f32 {
        if let Some(project) = &self.project {
            TempoMapConverter::beat_to_seconds_immut(
                &project.tempo_map,
                beat,
                project.settings.bpm,
            )
        } else {
            beat * 0.5 // fallback: 120 bpm
        }
    }
}
