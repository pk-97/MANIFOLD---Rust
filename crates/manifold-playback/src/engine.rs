use manifold_core::ClipId;
use manifold_core::PresetTypeId;
use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::math::BeatQuantizer;
use manifold_core::project::Project;
use manifold_core::tempo::TempoMapConverter;
use manifold_core::types::{LayerType, PlaybackState, TempoPointSource};
use manifold_core::{Beats, Bpm, LayerId, SceneId, Seconds};

use crate::live_clip_manager::LiveClipManager;
use crate::renderer::ClipRenderer;
use crate::scheduler::{ActiveClipRef, ClipScheduler};
use crate::session_state::SessionRuntime;

use ahash::{AHashMap, AHashSet};
use std::collections::HashMap;

// ─── Playback notification trait ───

/// Callback interface for playback events that affect the compositor/UI.
/// Port of C# IPlaybackNotifier.cs lines 9-18.
pub trait PlaybackNotifier {
    fn mark_compositor_dirty(&mut self);
    fn notify_generator_type_changed(&mut self, layer: &Layer, new_type: PresetTypeId);
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
    pub dt_seconds: Seconds,
    pub realtime_now: Seconds,
    pub pre_render_dt: Seconds,
    pub frame_count: u64,
    /// Fixed delta for export mode (0.0 = use real dt_seconds).
    /// Port of C# PlaybackController.exportFixedDeltaSeconds (line 42).
    pub export_fixed_dt: Seconds,
}

/// Output of a single engine tick.
///
/// No longer `Clone` as of automation recording (§5): `pending_gesture_commits`
/// holds `Box<dyn Command>`, which trait objects can't derive `Clone` for.
/// Nothing in the codebase cloned a `TickResult` (it's always moved through
/// once, ending at `reclaim_tick_result`), so dropping the derive is a
/// no-op for every existing call site.
#[derive(Debug, Default)]
pub struct TickResult {
    /// Active clips ready for compositing (lightweight references).
    pub ready_clips: Vec<ActiveClipRef>,
    pub compositor_dirty: bool,
    pub should_clear_compositor: bool,
    pub should_clear_feedback_buffer: bool,
    /// True when modulation (LFO drivers / ADSR envelopes) changed param_values
    /// this frame. The content thread uses this to send a project snapshot so
    /// the UI thread sees modulated slider values in real time.
    pub modulation_active: bool,
    /// Clip IDs that were stopped during this tick. Used by ContentPipeline
    /// to release per-owner GPU effect state (Feedback, Bloom, etc.),
    /// preventing unbounded GPU memory growth.
    pub stopped_clips: Vec<ClipId>,
    /// Video clips approaching the playhead that should be pre-warmed
    /// (decoder opened + first frame decoded before the clip becomes active).
    /// The content thread passes these to VideoRenderer::pre_warm_clips().
    pub prewarm_candidates:
        Option<std::collections::HashMap<String, crate::video_time::PrewarmCandidate>>,
    /// One `CommitRecordedGestureCommand` per automation recording gesture
    /// (§5) that finished this tick — built by
    /// `crate::automation::evaluate_all_automation`'s gesture-closure pass.
    /// The content thread runs each through `EditingService::execute` (the
    /// single undo entry per gesture §5/§11.6 requires) right after this
    /// tick returns, mirroring how `percussion_orchestrator.tick()` is
    /// already handed `&mut Project` + `&mut EditingService` synchronously
    /// in the same spot. Always empty while stopped/paused (recording only
    /// runs during `tick_playing`).
    pub pending_gesture_commits: Vec<Box<dyn manifold_editing::command::Command>>,
}

// ─── Playback Engine ───

/// Engine-agnostic playback logic. No platform dependencies.
/// All time comes via TickContext. Uses std math, logging via delegate.
#[allow(clippy::type_complexity)]
pub struct PlaybackEngine {
    // Transport state
    current_state: PlaybackState,
    current_time_double: f64,
    current_time: Seconds,
    current_beat: f64,
    playback_speed: f32,
    is_recording: bool,
    external_time_sync: bool,

    // Project reference
    project: Option<Project>,

    // Renderers
    renderers: Vec<Box<dyn ClipRenderer>>,

    // Active clip tracking
    active_clip_renderers: AHashMap<ClipId, usize>, // clip_id → renderer index
    active_clip_ids: AHashSet<ClipId>,
    preparing_clips: AHashSet<ClipId>,
    pending_pauses: AHashMap<ClipId, f64>, // clip_id → pause deadline
    looping_clip_ids: AHashSet<ClipId>,
    recently_started_times: AHashMap<ClipId, f64>, // clip_id → start realtime

    // Scheduling
    scheduler: ClipScheduler,

    // Live clip manager (MIDI phantom clips)
    live_clip_manager: Option<LiveClipManager>,

    // Session mode runtime (P2). Never serialized, never undo-wrapped — see
    // docs/SESSION_MODE_DESIGN.md §4. Sibling of `live_clip_manager`; always
    // present (unlike the live-clip manager, it holds no platform resources).
    session_runtime: SessionRuntime,

    // Live audio trigger edge-detection state (per-route armed flags). Drives
    // one-shot fires from incoming audio transients each tick.
    live_trigger_state: crate::live_trigger::LiveTriggerState,

    // D6 fire meter (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md`
    // P3c, BUG-082's fix): the current tick's shaped-signal capture for every
    // fire-mode config, written by `evaluate_modulation` (param gate cards)
    // and `tick_audio_triggers` (clip triggers) into ONE shared instance,
    // reset at the top of each `tick_playing`/`tick_non_playing`. `Copy`, so
    // resetting and reading it costs nothing — see
    // `manifold_core::audio_trigger::FireMeterCapture`.
    fire_meters: manifold_core::audio_trigger::FireMeterCapture,

    // Compositor
    compositor_dirty_deadline: f64,

    // Deferred sync flag — set by mark_sync_dirty(), consumed by tick/driver.
    // Port of C# PlaybackEngine.syncClipsDirty (lines 281-289).
    sync_clips_dirty: bool,

    // Live external tempo (set by driver from sync controllers each frame).
    // Port of C# PlaybackEngine lines 113-116.
    live_external_tempo: Option<(f32, TempoPointSource)>,

    // Drift correction. Port of C# PlaybackController.videoSyncInterval (line 33).
    video_sync_interval: Seconds,
    last_sync_time: Seconds,
    drift_correction_count: i32,
    is_export_mode: bool,

    // Clock state (for out-of-tick operations)
    last_realtime_now: f64,
    last_frame_count: u64,

    // Pre-allocated scratch buffers
    stop_buffer: Vec<ClipId>,
    /// Clips stopped during the current tick. Drained into TickResult::stopped_clips
    /// so the content pipeline can release per-owner GPU effect state.
    stopped_this_tick: Vec<ClipId>,
    ready_clips_list: Vec<ActiveClipRef>,
    timeline_active_scratch: Vec<ActiveClipRef>,
    /// Pre-allocated scratch for timeline active clip indices from get_active_clips_at_beat.
    active_indices_scratch: Vec<(usize, usize)>,
    /// Pre-allocated scratch for live slot refs (avoids per-frame Vec allocation).
    live_slot_refs_scratch: Vec<ActiveClipRef>,
    /// Pre-allocated scratch for session-slot refs — the third `sync_clips_to_time`
    /// input, alongside `timeline_active_scratch` / `live_slot_refs_scratch`.
    session_refs_scratch: Vec<ActiveClipRef>,
    /// Pre-allocated scratch: clip ids to force-evict this tick because a
    /// session loop wrap kept the same inner clip active across the boundary
    /// (§4 wrap-restart rule) — `compute_sync` diffs by clip_id and wouldn't
    /// otherwise restart it. Evicted via the engine's own `stop_clip` so the
    /// very next `compute_sync` call sees it as a fresh `to_start`.
    session_wrap_restart_scratch: Vec<ClipId>,
    /// Pre-allocated scratch for clips to start during sync (avoids per-sync Vec allocation).
    sync_start_scratch: Vec<ActiveClipRef>,
    /// Pre-allocated scratch for modulation active clip timing.
    modulation_timing_scratch: Vec<(Beats, Beats)>,
    /// §8 param triggers: which instances' own `audio_trigger` config fired
    /// this tick, from the most recent `evaluate_modulation` call. Drained by
    /// [`Self::take_trigger_pulses`] each tick (P2 plumbs this into the
    /// renderer's per-layer `audio_count`); reused as scratch between ticks to
    /// avoid a per-tick allocation.
    pending_trigger_pulses: Vec<crate::modulation::TriggerPulse>,
    /// PARAM_STEP_ACTIONS D5: last clip identity started on each layer
    /// (`timeline.layers` index → `ClipId`), the engine-side mirror of what
    /// `GeneratorRenderer::acquire_clip` tracks downstream per `LayerId`
    /// (`generator_renderer.rs:350-360`) — computed at the sole-authority
    /// level (`sync_clips_to_time`) instead of derived from renderer
    /// readiness. Never removed on stop (a "last" record, not "currently");
    /// only ever overwritten by a fresh `to_start`, so a stop-then-restart of
    /// the very same clip id is still detected as a change because
    /// `compute_sync`'s own diff (against `active_clip_ids`) already
    /// guarantees `to_start` only reports genuinely-new-to-the-layer starts.
    last_active_clip_id: AHashMap<i32, ClipId>,
    /// Layer indices with a clip-start edge accumulated since the last
    /// `evaluate_modulation` call — may span more than one `sync_clips_to_time`
    /// call (e.g. `play()`'s direct sync followed by the next tick's own sync)
    /// because it is only drained (never cleared) at tick's end, mirroring
    /// `pending_trigger_pulses`'/`take_trigger_pulses`'s drain-queue shape so
    /// an edge produced by an out-of-tick sync is never silently lost before
    /// modulation gets to see it. Zero per-frame allocation: capacity
    /// survives the clear-and-reassign at the end of `tick_playing`/
    /// `tick_non_playing`.
    clip_edge_layers: Vec<i32>,
    /// Latest per-send audio features, refreshed each tick by the content
    /// thread (which owns the capture/analysis worker) via
    /// [`Self::set_audio_snapshot`]. Empty when audio modulation is inactive, in
    /// which case the audio phase of the modulation pipeline is a no-op.
    audio_snapshot: manifold_core::audio_features::AudioFeatureSnapshot,
    /// Automation-lane override latch: params a live hand has touched since
    /// the last Back to Arrangement. Runtime-only, owned by the playback
    /// side (never the `Project`), never serialized, survives play/stop
    /// within a session. See `crate::automation` and
    /// `docs/AUTOMATION_LANES_DESIGN.md` §4.
    automation_latches: crate::automation::AutomationLatches,
    /// Global Automation Arm (§5): while on, a live touch on an automated
    /// param (while playing) records into its lane instead of latching an
    /// override. Runtime-only, owned by the playback side, never the
    /// `Project`, never serialized — same lifetime rule as
    /// `automation_latches`. Off by default.
    automation_armed: bool,
    /// In-flight recording gestures (§5). Runtime-only, owned by the
    /// playback side alongside `automation_latches`/`automation_armed`.
    automation_gestures: crate::automation::AutomationGestures,
    /// Frame count when timeline_active_scratch was last populated.
    /// Used to skip redundant re-queries within the same frame.
    timeline_query_frame: u64,
    became_ready_list: Vec<ClipId>,
    clips_to_stop_drift: Vec<ClipId>,
    prewarm_candidates: Vec<TimelineClip>,
    compositor_fallback_clips: Vec<ActiveClipRef>,

    // Prewarm state. Port of C# PlaybackEngine prewarm fields.
    next_prewarm_at: f64,
    last_prewarm_ids: AHashSet<String>,

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
    pub record_command_delegate:
        Option<Box<dyn Fn(Box<dyn manifold_editing::command::Command>) + Send>>,

    // Debug flag. Port of C# PlaybackEngine.showDebugLogs.
    pub show_debug_logs: bool,

    // Callback: fires each frame during playback after AdvanceTime.
    // Port of C# PlaybackController.OnTimeChanged (line 1149).
    pub on_time_changed: Option<Box<dyn Fn(Seconds) + Send>>,

    // Sort comparator scratch. Port of C# PlaybackEngine lines 211-214.
    // Rust uses closures for sorting — no static delegates needed, but
    // we keep the scratch buffers for zero-alloc iteration.
    to_pause_list: Vec<ClipId>,
}

impl PlaybackEngine {
    pub fn new(renderers: Vec<Box<dyn ClipRenderer>>) -> Self {
        Self {
            current_state: PlaybackState::Stopped,
            current_time_double: 0.0,
            current_time: Seconds::ZERO,
            current_beat: 0.0,
            playback_speed: 1.0,
            is_recording: false,
            external_time_sync: false,
            project: None,
            renderers,
            active_clip_renderers: AHashMap::with_capacity(32),
            active_clip_ids: AHashSet::with_capacity(32),
            preparing_clips: AHashSet::with_capacity(8),
            pending_pauses: AHashMap::with_capacity(8),
            looping_clip_ids: AHashSet::with_capacity(16),
            recently_started_times: AHashMap::with_capacity(8),
            scheduler: ClipScheduler::new(),

            live_clip_manager: None,
            session_runtime: SessionRuntime::new(),
            live_trigger_state: crate::live_trigger::LiveTriggerState::default(),
            fire_meters: manifold_core::audio_trigger::FireMeterCapture::default(),
            compositor_dirty_deadline: 0.0,
            sync_clips_dirty: false,
            live_external_tempo: None,
            video_sync_interval: Seconds(2.0),
            last_sync_time: Seconds::ZERO,
            drift_correction_count: 0,
            is_export_mode: false,
            last_realtime_now: 0.0,
            last_frame_count: 0,
            stop_buffer: Vec::with_capacity(16),
            stopped_this_tick: Vec::with_capacity(16),
            ready_clips_list: Vec::with_capacity(32),
            timeline_active_scratch: Vec::with_capacity(32),
            active_indices_scratch: Vec::with_capacity(32),
            live_slot_refs_scratch: Vec::with_capacity(8),
            session_refs_scratch: Vec::with_capacity(8),
            session_wrap_restart_scratch: Vec::with_capacity(4),
            sync_start_scratch: Vec::with_capacity(4),
            modulation_timing_scratch: Vec::with_capacity(64),
            pending_trigger_pulses: Vec::new(),
            last_active_clip_id: AHashMap::with_capacity(32),
            clip_edge_layers: Vec::with_capacity(8),
            audio_snapshot: manifold_core::audio_features::AudioFeatureSnapshot::default(),
            automation_latches: crate::automation::AutomationLatches::default(),
            automation_armed: false,
            automation_gestures: crate::automation::AutomationGestures::default(),
            timeline_query_frame: u64::MAX, // sentinel: never matches a real frame
            became_ready_list: Vec::with_capacity(8),
            clips_to_stop_drift: Vec::with_capacity(8),
            prewarm_candidates: Vec::with_capacity(32),
            compositor_fallback_clips: Vec::with_capacity(32),
            next_prewarm_at: 0.0,
            last_prewarm_ids: AHashSet::with_capacity(16),
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
            to_pause_list: Vec::with_capacity(8),
        }
    }

    // ─── Properties ───

    pub fn current_state(&self) -> PlaybackState {
        self.current_state
    }
    pub fn current_time_double(&self) -> f64 {
        self.current_time_double
    }
    pub fn current_time(&self) -> Seconds {
        self.current_time
    }
    pub fn current_beat(&self) -> Beats {
        Beats(self.current_beat)
    }

    /// Mutable access to the audio-feature snapshot the modulation pipeline
    /// reads. The content thread (which owns the capture/analysis worker) fills
    /// this in place each tick before `tick` — reusing the `Vec` capacity keeps
    /// the per-frame feed allocation-free. An empty snapshot disables the audio
    /// phase. Kept here (rather than passed via `TickContext`) because the
    /// engine, not the content thread, owns the project the pipeline mutates.
    pub fn audio_snapshot_mut(
        &mut self,
    ) -> &mut manifold_core::audio_features::AudioFeatureSnapshot {
        &mut self.audio_snapshot
    }
    /// Read the current per-send audio feature snapshot (for UI meters).
    pub fn audio_snapshot(&self) -> &manifold_core::audio_features::AudioFeatureSnapshot {
        &self.audio_snapshot
    }
    /// The D6 fire meter's per-config shaped-signal capture from the tick
    /// just run — content thread → `ContentState::fire_meters` → UI drawer
    /// meters. `Copy`, so reading it is free.
    pub fn fire_meters(&self) -> manifold_core::audio_trigger::FireMeterCapture {
        self.fire_meters
    }
    pub fn current_beat_f64(&self) -> f64 {
        self.current_beat
    }
    pub fn playback_speed(&self) -> f32 {
        self.playback_speed
    }
    pub fn is_playing(&self) -> bool {
        self.current_state == PlaybackState::Playing
    }
    pub fn is_recording(&self) -> bool {
        self.is_recording
    }
    pub fn external_time_sync(&self) -> bool {
        self.external_time_sync
    }
    pub fn is_export_mode(&self) -> bool {
        self.is_export_mode
    }
    pub fn video_sync_interval(&self) -> Seconds {
        self.video_sync_interval
    }
    pub fn active_clip_count(&self) -> usize {
        self.active_clip_renderers.len()
    }
    /// True when all active clips that need a prepare phase are ready.
    /// Used by export warmup to wait for video decoders.
    pub fn all_active_clips_ready(&self) -> bool {
        self.active_clip_renderers.iter().all(|(clip_id, &idx)| {
            !self.renderers[idx].needs_prepare_phase() || self.renderers[idx].is_clip_ready(clip_id)
        })
    }
    /// Block until all in-flight video decode jobs complete.
    /// Called in export loop to ensure decode keeps up with render speed.
    pub fn flush_pending_decodes(&mut self) {
        for renderer in &mut self.renderers {
            if renderer.has_pending_decodes() {
                renderer.flush_pending_decodes();
            }
        }
    }
    pub fn project(&self) -> Option<&Project> {
        self.project.as_ref()
    }
    pub fn project_mut(&mut self) -> Option<&mut Project> {
        self.project.as_mut()
    }
    pub fn live_clip_manager(&self) -> Option<&LiveClipManager> {
        self.live_clip_manager.as_ref()
    }
    pub fn live_clip_manager_mut(&mut self) -> Option<&mut LiveClipManager> {
        self.live_clip_manager.as_mut()
    }
    pub fn compositor_dirty_deadline(&self) -> f64 {
        self.compositor_dirty_deadline
    }
    /// The engine's real wall clock, as of the most recent `tick()` (or
    /// `set_clock()` for pre-first-tick callers) — the epoch every
    /// clip-lifecycle timing gate (`recently_started_times`, pending-pause,
    /// `compositor_dirty_deadline`) is anchored on. See F4 (CORE_ENGINE_MAP.md §5).
    pub fn last_realtime_now(&self) -> f64 {
        self.last_realtime_now
    }
    /// The realtime clock value `recently_started_times` was last stamped
    /// with for a given clip, if it has one. `None` once the compositor's
    /// gate has cleared the entry (`filter_ready_clips`'s retain).
    pub fn recently_started_time(&self, clip_id: &str) -> Option<f64> {
        self.recently_started_times.get(clip_id).copied()
    }

    // ─── Renderer access ───

    /// Replace a renderer at the given index (e.g., swap stub for real renderer after GPU init).
    pub fn replace_renderer(&mut self, index: usize, renderer: Box<dyn ClipRenderer>) {
        self.renderers[index] = renderer;
    }

    /// Split borrow: get renderers and project simultaneously.
    /// Needed because Rust can't borrow both `&mut self.renderers` and `&self.project`
    /// through a single `&mut self`.
    pub fn split_renderer_project(
        &mut self,
    ) -> (&mut Vec<Box<dyn ClipRenderer>>, Option<&Project>) {
        (&mut self.renderers, self.project.as_ref())
    }

    /// Mutable access to renderers for re-notification (e.g. after MutateProject).
    pub fn renderers_mut(&mut self) -> &mut Vec<Box<dyn ClipRenderer>> {
        &mut self.renderers
    }

    // ─── Lifecycle ───

    pub fn initialize(&mut self, mut project: Project) {
        // BUG-256: a project swap is a hard boundary for ALL runtime state
        // keyed by project-local identity. Renderers cache by `LayerId` /
        // `ClipId` and gate rebuilds on serialized per-project version
        // counters — both collide across two projects derived from the same
        // template, so without a full release the previous project's
        // generator instances keep serving the new project's layers (the
        // "locked to the first-loaded project" bug). Stop every clip (which
        // also drains the engine's own id-keyed maps) and release every
        // renderer's project-derived caches before the new project goes in.
        self.stop_all_clips();
        for renderer in &mut self.renderers {
            renderer.release_all();
        }

        // Ensure all runtime caches are populated regardless of how the project arrived.
        // LoadProject goes through the loader which already calls this, but NewProject
        // or programmatic construction may not. Redundant calls are safe (idempotent).
        project.on_after_deserialize();
        self.project = Some(project);
        // Runtime session state never survives a project swap (it is never
        // serialized) — a fresh/loaded project always starts arrangement-only.
        self.session_runtime.reset();

        self.current_time_double = 0.0;
        self.current_time = Seconds::ZERO;
        self.current_beat = 0.0;
        self.last_sync_time = Seconds::ZERO;
        self.drift_correction_count = 0;
        self.sync_clips_dirty = false;
        self.last_realtime_now = 0.0;
        self.last_frame_count = 0;
        self.timeline_query_frame = u64::MAX;

        // Notify all renderers of the new project (GAP-PLAY-5).
        // Port of C# PlaybackController.LoadProject → renderer.OnProjectLoaded().
        if let Some(ref project) = self.project {
            for renderer in &mut self.renderers {
                renderer.on_project_loaded(project);
            }
        }
    }

    /// Set the LiveClipManager after construction. Must be called before first tick.
    /// Port of C# PlaybackEngine.SetLiveClipManager (line 351).
    pub fn set_live_clip_manager(&mut self, mgr: LiveClipManager) {
        self.live_clip_manager = Some(mgr);
    }

    /// Reset the active clip window index. Call after bulk clip operations (undo/redo).
    pub fn reset_active_clip_window(&mut self) {}

    /// Update clock state for non-tick operations (Play, Stop, Seek).
    /// Port of C# PlaybackEngine.SetClock (lines 560-564).
    pub fn set_clock(&mut self, realtime_now: Seconds, frame_count: u64) {
        self.last_realtime_now = realtime_now.0;
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
        if !self.sync_clips_dirty {
            return false;
        }
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
        if self.current_state == PlaybackState::Playing {
            return;
        }
        self.current_state = PlaybackState::Playing;
        self.pending_pauses.clear();

        // Sync clips at current position (start clips that should be active)
        self.sync_clips_to_time();

        // Resume paused clips that were pre-warmed during Stop/LoadProject
        if !self.active_clip_renderers.is_empty() {
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
        // BUG-051: drop every audio-trigger edge-detector's armed state so a
        // stale "fired, not yet re-armed" flag can't suppress the first onset
        // next time transport starts. Both the live clip-trigger routes
        // (§1-7) and the §8 param-trigger holders (audio_trigger.edge +
        // ParameterAudioMod.trigger_edge) are runtime-only and never
        // serialized, so this is the only reset point that reaches them.
        self.live_trigger_state.clear();
        if let Some(project) = &mut self.project {
            crate::modulation::clear_all_trigger_edges(project);
        }
        // Transport stop stops all session playback and clears pending
        // launches (Ableton behavior). session_override is NOT cleared —
        // layers stay detached until an explicit Back to Arrangement (§4).
        self.session_runtime.on_transport_stop();
        self.current_time_double = 0.0;
        self.current_time = Seconds::ZERO;
        self.current_beat = 0.0;
        self.compositor_dirty_deadline = 0.0; // Force one more compositor update

        self.sync_clips_dirty = false;
    }

    pub fn pause(&mut self) {
        if self.current_state != PlaybackState::Playing {
            return;
        }
        self.current_state = PlaybackState::Paused;
        // Pause only seekable clips (generators render procedurally each frame)
        self.pause_active_clips();
    }

    pub fn set_time(&mut self, time_double: Seconds) {
        self.current_time_double = time_double.0;
        self.current_time = time_double;
        self.update_beat_from_time();
    }

    pub fn set_beat(&mut self, beat: Beats) {
        self.current_beat = beat.0;
    }

    /// Derive `current_time` / `current_time_double` from the current beat
    /// using the active tempo map. Call after `set_beat()` when the beat is
    /// the authoritative time source (external clock: Link, MIDI Clock).
    pub fn sync_time_from_beat(&mut self) {
        if let Some(project) = &mut self.project {
            let secs = TempoMapConverter::beat_to_seconds(
                &mut project.tempo_map,
                Beats(self.current_beat),
                project.settings.bpm,
            );
            self.current_time_double = secs.0.max(0.0);
            self.current_time = Seconds(self.current_time_double);
        }
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

    pub fn set_video_sync_interval(&mut self, interval: Seconds) {
        self.video_sync_interval = interval;
    }

    pub fn advance_time(&mut self, dt_seconds: Seconds) -> Seconds {
        self.current_time_double += dt_seconds.0;
        self.current_time = Seconds(self.current_time_double);
        self.update_beat_from_time();
        Seconds(self.current_time_double)
    }

    /// Set time from an external sync source (NudgeTime path).
    /// Port of C# PlaybackEngine.NudgeTime (lines 519-525).
    pub fn nudge_time(&mut self, time: Seconds) {
        self.current_time_double = time.0;
        self.current_time = time;
        self.update_beat_from_time();
        self.sync_project_bpm_from_current_beat();
    }

    /// Set time from a seek. Returns beat delta for feedback buffer clearing.
    /// Port of C# PlaybackEngine.SeekTo (lines 530-538).
    pub fn seek_to(&mut self, time: Seconds) -> f32 {
        let old_beat = self.current_beat;
        self.set_time(Seconds(time.0.max(0.0)));
        self.sync_project_bpm_from_current_beat();

        // Session slots are beat-anchored and stateless, so a seek never
        // stops them — but pending launches must retarget to the next
        // quantize boundary after the new position (§4), or a jump could
        // fire one off-grid (or, if the new position is already past the
        // old target, instantly).
        self.session_runtime.on_seek(self.current_beat);

        // Clear live clips on large seek
        let beat_delta = (self.current_beat - old_beat).abs();
        // Note: live_clip_manager.clear_on_seek needs a stop callback.
        // The engine's stop_clip handles renderer cleanup, but we can't call it here
        // due to borrow conflict. Instead, collect IDs and stop after the borrow.
        if beat_delta > 1.0
            && let Some(mgr) = &mut self.live_clip_manager
        {
            let ids_to_stop: Vec<ClipId> = mgr
                .live_slots_list()
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
                self.stopped_this_tick.push(id.clone());
            }
        }

        // Re-sync clips at new position — unconditional, matching Unity's
        // PlaybackController.Seek() which always calls SyncClipsToTime() + SeekActiveClips()
        // regardless of playback state. This is what makes scrub-while-stopped work.
        self.sync_clips_to_time();
        self.seek_active_clips();

        // Mark compositor dirty so the stopped-state tick renders the new frame.
        // Port of Unity SeekActiveClips() setting compositorDirtyDeadline.
        self.compositor_dirty_deadline = self.last_realtime_now + COMPOSITOR_DIRTY_TIME as f64;

        beat_delta as f32
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
    #[must_use]
    pub fn tick(&mut self, ctx: TickContext) -> TickResult {
        if self.is_ticking {
            return TickResult::default();
        }
        self.is_ticking = true;
        self.stopped_this_tick.clear();

        if self.project.is_none() {
            self.is_ticking = false;
            return TickResult::default();
        }

        self.last_realtime_now = ctx.realtime_now.0;
        self.last_frame_count = ctx.frame_count;

        // D6 fire meter (BUG-109 fix): ONE reset per tick, before either
        // branch's evaluators write this tick's levels — param gate cards
        // via `evaluate_modulation` (both branches), clip triggers via
        // `tick_audio_triggers` (playing, step 3b) or the meter-only
        // conditioning walk (non-playing, step 1b).
        self.fire_meters = manifold_core::audio_trigger::FireMeterCapture::default();

        // ── Phase 1 & 2 (beat derivation + tempo recording) ──
        // Handled by ContentThread BEFORE engine.tick() — matching Unity where
        // PlaybackController does this at its level before the engine's per-state logic.
        // ContentThread calls derive_external_beat(), update_recording_session_state(),
        // and apply_resolved_tempo() between tick_sync_controllers() and engine.tick().

        // ── Phase 3: Shared pre-branch (all states) ──
        // Port of C# PlaybackController.Update lines 1102-1112.
        self.sync_project_bpm_from_current_beat();
        self.process_pending_pauses(ctx.realtime_now);
        self.check_preparing_clips();

        // ── Phase 4: Branch on playback state ──
        let mut result = if self.current_state == PlaybackState::Playing {
            self.tick_playing(ctx)
        } else {
            self.tick_non_playing(ctx)
        };

        // Drain stopped clips into TickResult for per-owner GPU effect state cleanup.
        if !self.stopped_this_tick.is_empty() {
            result.stopped_clips = self.stopped_this_tick.drain(..).collect();
        }

        self.is_ticking = false;
        result
    }

    /// §8 param triggers: drain this tick's fired `audio_trigger` pulses
    /// (P1's evaluator output) for the caller to fold into the renderer's
    /// per-layer `audio_count` (P2). Leaves the scratch Vec's capacity intact
    /// for reuse next tick.
    pub fn take_trigger_pulses(&mut self) -> Vec<crate::modulation::TriggerPulse> {
        std::mem::take(&mut self.pending_trigger_pulses)
    }

    /// Reclaim the ready_clips buffer from a consumed TickResult.
    /// Call after the frame is done with the TickResult to preserve
    /// the pre-allocated buffer for the next tick (zero allocation).
    pub fn reclaim_tick_result(&mut self, mut result: TickResult) {
        // Take the ready_clips Vec back — its capacity survives for reuse.
        result.ready_clips.clear();
        self.ready_clips_list = result.ready_clips;
        // Also reclaim stopped_clips buffer.
        result.stopped_clips.clear();
        self.stopped_this_tick = result.stopped_clips;
    }

    /// Playing-state tick. Matches C# PlaybackController.Update lines 1135-1218.
    fn tick_playing(&mut self, ctx: TickContext) -> TickResult {
        // 1. Clear deferred sync flag — SyncClipsToTime below handles it.
        //    Port of C# line 1138.
        self.consume_sync_dirty();

        // 2. Advance time (unless external sync source is the clock authority).
        //    Port of C# lines 1141-1150.
        if !self.external_time_sync {
            let frame_delta = if self.is_export_mode && ctx.export_fixed_dt.0 > 0.0 {
                ctx.export_fixed_dt
            } else {
                ctx.dt_seconds
            };
            self.advance_time(Seconds(frame_delta.0 * self.playback_speed as f64));
            self.sync_project_bpm_from_current_beat();

            // Fire on_time_changed callback. Port of C# line 1149.
            if let Some(ref cb) = self.on_time_changed {
                cb(self.current_time);
            }
        }

        // 3b. Live audio triggers — fire one-shot clips from incoming transients
        //     and expire elapsed ones, before the sync below picks up the new
        //     slots (same-frame, matching the MIDI activation above). Uses the
        //     audio snapshot set by the content thread before this tick.
        if self.tick_audio_triggers(ctx.realtime_now.0, ctx.dt_seconds) {
            self.mark_compositor_dirty(ctx.realtime_now);
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

        // 6b. Sample automation lanes — a tier-1 hand (base writer), sampled
        //     from the arrangement. Must land BEFORE the modulation base→value
        //     reset just below it, not after — automation is not a fifth
        //     modulation phase. Runs whenever the transport is playing
        //     (tick_playing only — export drives the transport in Playing
        //     state too, so export sampling falls out for free; when stopped,
        //     tick_non_playing never calls this, so lanes don't write and
        //     params hold). See docs/AUTOMATION_LANES_DESIGN.md §1, §3.
        //     While `automation_armed`, a touched param records into its
        //     lane instead of latching (§5); `pending_gesture_commits`
        //     carries one `CommitRecordedGestureCommand` per gesture that
        //     closed this tick, for the content thread to run through
        //     `EditingService` right after this tick returns.
        let (automation_dirty, pending_gesture_commits) = if let Some(project) = &mut self.project
        {
            crate::automation::evaluate_all_automation(
                project,
                Beats(self.current_beat),
                &mut self.automation_latches,
                self.automation_armed,
                &mut self.automation_gestures,
            )
        } else {
            (false, Vec::new())
        };

        // 7. Evaluate modulation pipeline (LFO drivers + ADSR envelopes).
        //    Port of C# DriverController.Update() [ExecutionOrder 50, after PlaybackController].
        let mut timing = std::mem::take(&mut self.modulation_timing_scratch);
        let mut pulses = std::mem::take(&mut self.pending_trigger_pulses);
        // PARAM_STEP_ACTIONS D5: drain (not clear-at-top) the clip-edge queue
        // accumulated by every `sync_clips_to_time` call since the last time
        // modulation consumed it — this tick's own step 4 sync, plus any
        // out-of-tick sync (`play()`/`seek_to()`) that ran since. Cleared only
        // after this call so an edge from an out-of-tick sync is never lost.
        let mut clip_edges = std::mem::take(&mut self.clip_edge_layers);
        let audio = &self.audio_snapshot;
        // D6 fire meter: this tick's single reset lives at the top of
        // `tick()`, before step 3b's clip-trigger push (BUG-109) — nothing
        // to reset here.
        let modulation_dirty = if let Some(project) = &mut self.project {
            crate::modulation::evaluate_modulation(
                project,
                Beats(self.current_beat),
                ctx.dt_seconds,
                audio,
                &mut timing,
                &mut pulses,
                &clip_edges,
                &mut self.fire_meters,
            )
        } else {
            false
        };
        self.modulation_timing_scratch = timing;
        self.pending_trigger_pulses = pulses;
        clip_edges.clear();
        self.clip_edge_layers = clip_edges;
        // Automation folds into the same compositor-dirty path modulation
        // uses — a lane write is just as much a reason to re-send the UI
        // snapshot as a driver/envelope write.
        let modulation_dirty = automation_dirty || modulation_dirty;
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

        let compositor_dirty =
            !ready.is_empty() || ctx.realtime_now.0 < self.compositor_dirty_deadline;
        let should_clear = ready.is_empty() && !self.has_pending_clip_state();

        // 10. Lookahead prewarm — engine computes candidates, caller executes pool pre-warm.
        //     Port of C# line 1217: UpdateLookaheadPrewarm(force: false).
        //     Candidates are returned in TickResult for the caller (app.rs) to act on.
        let prewarm = self.compute_prewarm_candidates(false);

        TickResult {
            ready_clips: ready,
            compositor_dirty,
            should_clear_compositor: should_clear,
            should_clear_feedback_buffer: false,
            modulation_active: modulation_dirty,
            stopped_clips: Vec::new(), // Populated by tick() after this returns
            prewarm_candidates: prewarm,
            pending_gesture_commits,
        }
    }

    /// Non-playing (paused/stopped) tick. Matches C# PlaybackController.Update lines 1114-1133.
    fn tick_non_playing(&mut self, ctx: TickContext) -> TickResult {
        // 1. Flush deferred sync from MIDI events.
        //    Port of C# lines 1117-1120.
        if self.consume_sync_dirty() {
            self.sync_clips_to_time();
            self.seek_active_clips();
        }

        // 2. Keep active clip playback rates aligned.
        //    Port of C# line 1122.
        self.update_active_clip_playback_rates();

        // 2b. Live audio triggers, meter-only (BUG-109 §7.1 item 2). A clip
        //     trigger never FIRES while stopped — one-shot expiry is
        //     beat-based and the clock is frozen — but a performer tuning a
        //     trigger at soundcheck (transport stopped, track through the
        //     tap) still needs to see the shaped signal move. Runs the same
        //     `condition()` walk `tick_audio_triggers` runs at step 3b while
        //     playing, without ever advancing the fire edge, so resuming
        //     playback can't inherit a fire decided while stopped.
        if let Some(project) = self.project.as_ref()
            && project.has_active_clip_triggers()
        {
            self.live_trigger_state.evaluate_meter_only(
                &self.audio_snapshot,
                &project.audio_setup,
                &project.timeline.layers,
                ctx.dt_seconds,
                &mut self.fire_meters,
            );
        }

        // 3. Evaluate modulation pipeline even when stopped (for scrub preview / inspector).
        //    Port of C# DriverController — runs in all states.
        let mut timing = std::mem::take(&mut self.modulation_timing_scratch);
        let mut pulses = std::mem::take(&mut self.pending_trigger_pulses);
        // PARAM_STEP_ACTIONS D5: see the matching comment in `tick_playing` —
        // same drain-queue shape, so a scrub-while-stopped's own sync (step 1
        // above, when dirty) still reaches modulation this tick.
        let mut clip_edges = std::mem::take(&mut self.clip_edge_layers);
        // D6 fire meter: this tick's single reset lives at the top of
        // `tick()` — nothing to reset here. Step 2b above already pushed
        // clip-trigger levels; this evaluate_modulation call pushes param
        // gate-card levels into the same capture.
        let dirty = {
            let audio = &self.audio_snapshot;
            if let Some(project) = &mut self.project {
                crate::modulation::evaluate_modulation(
                    project,
                    Beats(self.current_beat),
                    ctx.dt_seconds,
                    audio,
                    &mut timing,
                    &mut pulses,
                    &clip_edges,
                    &mut self.fire_meters,
                )
            } else {
                false
            }
        };
        if dirty {
            self.mark_compositor_dirty(ctx.realtime_now);
        }
        let modulation_dirty = dirty;
        self.modulation_timing_scratch = timing;
        self.pending_trigger_pulses = pulses;
        clip_edges.clear();
        self.clip_edge_layers = clip_edges;

        // 4. Filter ready clips for compositor.
        //    Port of C# UpdateCompositor (lines 1126-1132).
        //    Only runs while compositor dirty deadline is active or generators are running.
        let has_active_clips = !self.active_clip_renderers.is_empty();
        let compositor_dirty =
            ctx.realtime_now.0 < self.compositor_dirty_deadline || has_active_clips;

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
            modulation_active: modulation_dirty,
            stopped_clips: Vec::new(), // Populated by tick() after this returns
            prewarm_candidates: None,
            // Recording only runs during tick_playing (§5's "armed + playing
            // + touched") — stopped/paused never opens or closes a gesture.
            pending_gesture_commits: Vec::new(),
        }
    }

    /// Query timeline for active clips at current beat, populating timeline_active_scratch.
    /// Uses split borrows to avoid cloning the project.
    /// Stamps `timeline_query_frame` so callers later in the same frame can skip re-query.
    fn query_active_timeline_clips(&mut self) {
        // Step 1: ensure layer sort caches are up-to-date (needs &mut project)
        if let Some(p) = &mut self.project {
            p.timeline.ensure_layers_sorted();
        }

        // Step 2: query active clips and build lightweight refs
        // (split borrow: project.timeline vs self.timeline_active_scratch/active_indices_scratch)
        self.timeline_active_scratch.clear();
        if let Some(project) = &mut self.project {
            let beat = Beats(self.current_beat);
            project
                .timeline
                .get_active_clips_at_beat_ref(beat, &mut self.active_indices_scratch);
        }
        if let Some(project) = &self.project {
            for (li, ci) in &self.active_indices_scratch {
                let Some(layer) = project.timeline.layers.get(*li) else {
                    continue;
                };
                // Arrangement suppression (§6): a layer detached into session
                // mode plays ONLY session content. Skipping it here is the
                // entire integration — the scheduler diff then stops
                // arrangement clips and starts session clips with no further
                // changes downstream (compositor/effects/LED don't know or
                // care where an active clip came from).
                if self.session_runtime.is_overridden(&layer.layer_id) {
                    continue;
                }
                if let Some(clip) = layer.clips.get(*ci) {
                    self.timeline_active_scratch.push(ActiveClipRef {
                        clip_id: clip.id.clone(),
                        layer_index: *li as i32,
                        clip_index: *ci as u32,
                        start_beat: clip.start_beat,
                        duration_beats: clip.duration_beats,
                        is_looping: clip.is_looping,
                        is_video: !clip.video_clip_id.is_empty(),
                    });
                }
            }
        }
        self.timeline_query_frame = self.last_frame_count;
    }

    // ─── Clip lifecycle ───

    pub fn start_clip(&mut self, clip: &TimelineClip, realtime_now: Seconds, layer_index: i32) {
        // Fix 6: Never start clips on group layers
        if let Some(project) = &self.project
            && let Some(li) = project.timeline.layer_index_for_id(&clip.layer_id)
            && let Some(layer) = project.timeline.layers.get(li)
            && layer.layer_type == LayerType::Group
        {
            return;
        }

        // Find renderer
        let renderer_idx = self.renderers.iter().position(|r| r.can_handle(clip));
        if let Some(idx) = renderer_idx {
            let layers = self
                .project
                .as_ref()
                .map_or(&[] as &[_], |p| &p.timeline.layers);
            let success =
                self.renderers[idx].start_clip(clip, self.current_time, layers, layer_index);
            if success {
                self.active_clip_renderers.insert(clip.id.clone(), idx);
                self.active_clip_ids.insert(clip.id.clone());
                self.recently_started_times
                    .insert(clip.id.clone(), realtime_now.0);

                if clip.is_looping {
                    self.looping_clip_ids.insert(clip.id.clone());
                }

                // Pending pause for video renderers
                if self.renderers[idx].needs_pending_pause() {
                    self.pending_pauses
                        .insert(clip.id.clone(), realtime_now.0 + PENDING_PAUSE_DELAY as f64);
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
        // Track stopped clip for per-owner GPU effect state cleanup.
        self.stopped_this_tick.push(ClipId::new(clip_id));
        // Notify live clip manager so it can track which live slots are still active.
        // Port of C# PlaybackEngine.StopClip → liveClipManager.NotifyClipStopped (line 684).
        if let Some(mgr) = &mut self.live_clip_manager {
            mgr.notify_clip_stopped(clip_id);
        }
    }

    pub fn stop_all_clips(&mut self) {
        self.stop_buffer.clear();
        self.stop_buffer
            .extend(self.active_clip_ids.iter().cloned());
        for clip_id in &self.stop_buffer {
            if let Some(renderer_idx) = self.active_clip_renderers.remove(clip_id.as_str()) {
                self.renderers[renderer_idx].stop_clip(clip_id);
            }
        }
        // Track all stopped clips for per-owner GPU effect state cleanup.
        self.stopped_this_tick
            .extend(self.stop_buffer.iter().cloned());
        self.active_clip_ids.clear();
        self.preparing_clips.clear();
        self.pending_pauses.clear();
        self.looping_clip_ids.clear();
        self.recently_started_times.clear();
    }

    // ─── Pending pauses ───

    pub fn process_pending_pauses(&mut self, realtime_now: Seconds) {
        let expired: Vec<ClipId> = self
            .pending_pauses
            .iter()
            .filter(|(_, deadline)| realtime_now.0 >= **deadline)
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
            self.compositor_dirty_deadline = realtime_now.0 + COMPOSITOR_DIRTY_TIME as f64;
        }
    }

    // ─── Compositor ───

    pub fn mark_compositor_dirty(&mut self, realtime_now: Seconds) {
        self.compositor_dirty_deadline = realtime_now.0 + COMPOSITOR_DIRTY_TIME as f64;
    }

    /// Mark the compositor dirty using the most recent tick's realtime clock.
    ///
    /// Use this from command handlers that change visible project state
    /// (mute/solo/blend/opacity/effect edits) outside the playing tick. While
    /// paused, the compositor only re-renders while this deadline is in the
    /// future (or an active clip renderer keeps it busy), so a mutation that
    /// isn't accompanied by this mark won't show until playback advances time.
    /// `mark_compositor_dirty(Seconds::ZERO)` does NOT work for this: the
    /// realtime clock is seconds-since-start, so a `0.05` deadline is always in
    /// the past. This anchors off `last_realtime_now`, matching the seek path.
    pub fn mark_compositor_dirty_now(&mut self) {
        self.compositor_dirty_deadline = self.last_realtime_now + COMPOSITOR_DIRTY_TIME as f64;
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
            if clip.is_looping && clip.loop_duration_beats > Beats::ZERO {
                let bpm = self
                    .project
                    .as_ref()
                    .map(|p| p.settings.bpm.0)
                    .unwrap_or(120.0);
                let spb = 60.0_f32 / bpm.max(20.0);
                let clip_start_time = clip.start_beat.as_f32() * spb;
                let loop_dur_seconds = clip.loop_duration_beats.as_f32() * spb;
                let media_length = self.renderers[renderer_idx].get_clip_media_length(clip_id);
                let video_time = crate::video_time::compute_video_time(
                    self.current_time.as_f32(),
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
            let current_bpm = self
                .project
                .as_ref()
                .map(|p| p.settings.bpm.0)
                .unwrap_or(120.0);
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
    pub fn get_timeline_active_clips_at_current_beat(&mut self) -> &[ActiveClipRef] {
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

        let bpm = self
            .project
            .as_ref()
            .map(|p| p.settings.bpm.0)
            .unwrap_or(120.0);
        let spb = 60.0_f32 / bpm.max(20.0);
        let min_remaining_beats = if spb > 0.0 {
            MIN_START_REMAINING_TIME / spb
        } else {
            MIN_START_REMAINING_TIME
        };

        self.live_slot_refs_scratch.clear();
        if let Some(mgr) = &self.live_clip_manager {
            mgr.fill_live_slot_refs(&mut self.live_slot_refs_scratch);
        }

        // Third reference source (§4/§9): an input to this sole authority,
        // never a parallel path. May evict a clip via `stop_clip` (the same
        // primitive this function's own to_stop loop uses) to force a
        // same-clip loop-wrap restart before the diff below runs.
        self.resolve_session_refs();

        let sync_result = self.scheduler.compute_sync(
            self.current_time,
            Beats(self.current_beat),
            &self.timeline_active_scratch,
            &self.live_slot_refs_scratch,
            &self.session_refs_scratch,
            &self.active_clip_ids,
            &self.looping_clip_ids,
            Beats::from_f32(min_remaining_beats),
        );

        for clip_id in &sync_result.to_stop {
            self.stop_clip(clip_id);
        }

        // F4: `start_clip` below is stamped with `self.last_realtime_now`, not
        // a zero epoch. `sync_clips_to_time` is called from outside `tick()`
        // (play/seek/session commands), but `last_realtime_now` is still the
        // real wall clock — `tick()` stamps it unconditionally every frame
        // regardless of playback state (engine.rs `last_realtime_now =
        // ctx.realtime_now.0`), and `set_clock` covers pre-first-tick callers.
        // A `Seconds::ZERO` epoch here would silently defeat three downstream
        // gates fed by `start_clip`'s `realtime_now` parameter: the
        // compositor-exclusion window (`recently_started_times`,
        // `should_exclude_recently_started`), the pending-pause deadline, and
        // `mark_compositor_dirty`'s own deadline — all three compare against
        // `last_realtime_now`, so a 0.0 stamp is already "expired" the moment
        // the engine has been running longer than the gate window. Matches
        // `check_preparing_clips`'s video re-anchor and the seek path's
        // `compositor_dirty_deadline` (both anchor on `last_realtime_now`).
        // Resolve full TimelineClip from project (timeline) or live_clip_manager.
        // Swap scratch in to break borrow — capacity preserved across frames.
        let mut starts = std::mem::take(&mut self.sync_start_scratch);
        starts.clear();
        starts.extend(sync_result.to_start.iter().cloned());
        for entry in &starts {
            // PARAM_STEP_ACTIONS D5: record the clip-edge at scheduler-decision
            // time — the engine's own notion of "this layer just started a
            // clip" — never gated on whether a renderer later accepts it
            // (that's the acquire_clip-level readiness divergence D5 accepts
            // by design). `to_start` already guarantees this clip_id was not
            // in `active_clip_ids` a moment ago, so the identity comparison
            // below is almost always true; it's kept explicit (rather than
            // pushing unconditionally) so `last_active_clip_id` is a genuine
            // before/after diff, not just a to_start mirror.
            if self.last_active_clip_id.get(&entry.layer_index) != Some(&entry.clip_id) {
                self.clip_edge_layers.push(entry.layer_index);
            }
            self.last_active_clip_id
                .insert(entry.layer_index, entry.clip_id.clone());

            let clip = if entry.is_live_slot() {
                self.live_clip_manager
                    .as_ref()
                    .and_then(|mgr| mgr.find_live_slot_clip(&entry.clip_id))
                    .cloned()
            } else if entry.is_session_slot() {
                self.resolve_session_clip_for_start(entry)
            } else {
                self.project
                    .as_ref()
                    .and_then(|p| p.timeline.layers.get(entry.layer_index as usize))
                    .and_then(|l| l.clips.get(entry.clip_index as usize))
                    .cloned()
            };
            if let Some(ref clip) = clip {
                self.start_clip(clip, Seconds(self.last_realtime_now), entry.layer_index);
            }
        }
        self.sync_start_scratch = starts;

        if !sync_result.to_stop.is_empty() {
            // F4: anchor on `last_realtime_now`, mirroring the seek path
            // (`seek_to`'s `compositor_dirty_deadline` update) — never a zero
            // epoch, which is already in the past the instant the engine has
            // been running longer than `COMPOSITOR_DIRTY_TIME`.
            self.compositor_dirty_deadline = self
                .compositor_dirty_deadline
                .max(self.last_realtime_now + COMPOSITOR_DIRTY_TIME as f64);
        }

        // Reclaim buffers for reuse on the next sync call (zero allocation).
        self.scheduler.reclaim(sync_result);
    }

    // ─── Session mode resolution (P2) ───

    /// Fill `session_refs_scratch` (the third `sync_clips_to_time` input) from
    /// `SessionRuntime`, and apply any wrap-restart evictions it reports.
    ///
    /// The eviction step mirrors `seek_to`'s existing "collect ids, stop after
    /// the borrow" pattern (this file, `seek_to`): `SessionRuntime::resolve_refs`
    /// only has `&Project`/`&Timeline` access and cannot call `self.stop_clip`
    /// itself, so it reports the ids to evict and this method applies them
    /// with the engine's own `stop_clip` — the same primitive the to_stop loop
    /// in `sync_clips_to_time` already uses. This keeps `sync_clips_to_time`
    /// the sole authority: the eviction only pre-empties the very next
    /// `compute_sync` diff, it never bypasses it.
    fn resolve_session_refs(&mut self) {
        self.session_refs_scratch.clear();
        self.session_wrap_restart_scratch.clear();
        let current_beat = self.current_beat;
        if let Some(project) = &self.project {
            self.session_runtime.resolve_refs(
                current_beat,
                &project.session,
                &project.timeline,
                &mut self.session_refs_scratch,
                &mut self.session_wrap_restart_scratch,
            );
        }
        if !self.session_wrap_restart_scratch.is_empty() {
            let ids = std::mem::take(&mut self.session_wrap_restart_scratch);
            for clip_id in &ids {
                self.stop_clip(clip_id);
            }
            self.session_wrap_restart_scratch = ids;
        }
    }

    /// Resolve the full `TimelineClip` for a session-slot `ActiveClipRef` at
    /// start time (rare, per-event — mirrors the live-slot/timeline arms just
    /// above it). Delegates to `SessionRuntime::resolve_clip_for_start`, which
    /// rebases the resolved clip's `start_beat` to the ref's already-global
    /// value (the inner clip's stored `start_beat` is sequence-relative and
    /// must never reach `start_clip`/video-time math directly).
    fn resolve_session_clip_for_start(&self, entry: &ActiveClipRef) -> Option<TimelineClip> {
        let project = self.project.as_ref()?;
        let layer_id = &project.timeline.layers.get(entry.layer_index as usize)?.layer_id;
        self.session_runtime
            .resolve_clip_for_start(&project.session, layer_id, &entry.clip_id, entry.start_beat)
    }

    // ─── Session mode commands (P2, §5) ───

    /// Launch a session slot, or — if the (layer, scene) cell is empty —
    /// issue the sparse-grid stop for that layer (§5: "empty slot cells
    /// don't exist"). Starts the transport first if it was stopped, and in
    /// that case launches immediately rather than waiting for the next
    /// quantize boundary (§4: "the grid is dead on first click for
    /// timeline-free users" otherwise).
    pub fn session_launch_slot(&mut self, layer_id: LayerId, scene_id: SceneId) {
        let has_slot = self
            .project
            .as_ref()
            .is_some_and(|p| p.session.get_slot(&layer_id, &scene_id).is_some());
        let immediate = !self.is_playing();
        if immediate {
            self.play();
        }
        let current_beat = self.current_beat;
        if has_slot {
            self.session_runtime
                .launch_slot(layer_id, scene_id, current_beat, immediate);
        } else {
            self.session_runtime.stop_slot(layer_id, current_beat, immediate);
        }
        // Direct re-sync (not just `mark_sync_dirty`), matching `play`/`seek_to`:
        // this is a dedicated playback-affecting API call, not a generic
        // project edit — a quantized launch's *pending* entry still waits for
        // its beat, but an immediate one must resolve now, not one tick later.
        self.sync_clips_to_time();
    }

    /// Quantized stop for one layer's session slot. `session_override`
    /// persists (§5/§12) — the layer goes black, it does not fall back to
    /// the arrangement.
    pub fn session_stop_slot(&mut self, layer_id: LayerId) {
        let current_beat = self.current_beat;
        self.session_runtime.stop_slot(layer_id, current_beat, false);
        self.sync_clips_to_time();
    }

    /// Launch every slot in `scene_id`; layers currently playing a session
    /// slot with no slot in this scene get a quantized stop (Ableton "stop
    /// other tracks" default, §5). Starts the transport first if stopped,
    /// same as `session_launch_slot`.
    pub fn session_launch_scene(&mut self, scene_id: SceneId) {
        let immediate = !self.is_playing();
        if immediate {
            self.play();
        }
        let current_beat = self.current_beat;
        if let Some(project) = &self.project {
            self.session_runtime
                .launch_scene(&scene_id, &project.session, current_beat, immediate);
        }
        self.sync_clips_to_time();
    }

    /// Quantized stop of every currently-playing (or about-to-play) session
    /// slot. Distinct from a full transport stop: `session_override` is
    /// untouched, exactly like a single `session_stop_slot`.
    pub fn session_stop_all(&mut self) {
        let current_beat = self.current_beat;
        self.session_runtime.stop_all(current_beat);
        self.sync_clips_to_time();
    }

    /// Immediate (not quantized): clears `session_override` for the layer
    /// (or every layer if `None`) and stops its playing slot. Timeline clips
    /// resume via the normal `sync_clips_to_time` call this method itself
    /// makes — not deferred to the next tick.
    pub fn session_back_to_arrangement(&mut self, layer_id: Option<LayerId>) {
        self.session_runtime.back_to_arrangement(layer_id.as_ref());
        self.sync_clips_to_time();
    }

    /// Set the global session launch quantize (0 = launch immediately).
    pub fn session_set_quantize(&mut self, beats: Beats) {
        self.session_runtime.set_quantize(beats);
    }

    /// Read-only access to session runtime state (UI snapshot / tests).
    pub fn session_runtime(&self) -> &SessionRuntime {
        &self.session_runtime
    }

    /// Automation lanes' "Back to Arrangement": clears every override latch
    /// (global — one action, not per-layer), resuming every automated
    /// param's lane on the next tick. Not a project mutation — no undo entry
    /// (`ContentCommand::AutomationBackToArrangement` is handled directly on
    /// the content thread, same shape as `SessionBackToArrangement`).
    pub fn automation_back_to_arrangement(&mut self) {
        self.automation_latches.clear();
    }

    /// Read-only access to the automation override latch (UI snapshot: the
    /// "lit red" Back to Arrangement affordance, and per-lane overridden
    /// state — P4).
    pub fn automation_latches(&self) -> &crate::automation::AutomationLatches {
        &self.automation_latches
    }

    /// Toggle the global Automation Arm (§5). Runtime-only, not a project
    /// mutation — no undo entry (`ContentCommand::AutomationSetArmed` is
    /// handled directly on the content thread, same shape as
    /// `automation_back_to_arrangement`).
    pub fn set_automation_armed(&mut self, armed: bool) {
        self.automation_armed = armed;
    }

    /// Read-only access to the Automation Arm state (UI snapshot: the
    /// transport-bar arm button's lit/unlit state — P4).
    pub fn automation_armed(&self) -> bool {
        self.automation_armed
    }

    // ─── Time/Tempo math ───

    fn update_beat_from_time(&mut self) {
        if let Some(project) = &mut self.project {
            self.current_beat = TempoMapConverter::seconds_to_beat_f64(
                &mut project.tempo_map,
                self.current_time_double,
                project.settings.bpm,
            );
        }
    }

    pub fn time_to_timeline_beat(&mut self, time_seconds: Seconds) -> Beats {
        if let Some(project) = &mut self.project {
            TempoMapConverter::seconds_to_beat(
                &mut project.tempo_map,
                time_seconds,
                project.settings.bpm,
            )
        } else {
            Beats(time_seconds.0 * 2.0) // fallback: 120 bpm
        }
    }

    pub fn beat_to_timeline_time(&mut self, beat: Beats) -> Seconds {
        if let Some(project) = &mut self.project {
            TempoMapConverter::beat_to_seconds(&mut project.tempo_map, beat, project.settings.bpm)
        } else {
            Seconds(beat.as_f32() as f64 * 0.5) // fallback: 120 bpm
        }
    }

    /// Immutable version of beat_to_timeline_time. Used by StemAudioController
    /// which borrows engine immutably for sync.
    pub fn beat_to_timeline_time_immut(&self, beat: Beats) -> Seconds {
        if let Some(project) = &self.project {
            TempoMapConverter::beat_to_seconds_immut(&project.tempo_map, beat, project.settings.bpm)
        } else {
            Seconds(beat.as_f32() as f64 * 0.5) // fallback: 120 bpm
        }
    }

    pub fn get_seconds_per_beat(&mut self) -> f32 {
        self.get_seconds_per_beat_at_beat(Beats(self.current_beat))
    }

    /// Get seconds-per-beat at a given beat. Checks live external tempo first.
    /// Returns a ratio (f32), not a Seconds value.
    /// Port of C# PlaybackEngine.GetSecondsPerBeatAtBeat (lines 1372-1387).
    pub fn get_seconds_per_beat_at_beat(&mut self, beat: Beats) -> f32 {
        // Priority 1: live external tempo (Link/MIDI Clock)
        if let Some((live_bpm, _)) = self.try_get_live_external_tempo() {
            return TempoMapConverter::seconds_per_beat_from_bpm(live_bpm);
        }
        // Priority 2: tempo map
        if let Some(project) = &mut self.project {
            let bpm = project
                .tempo_map
                .get_bpm_at_beat(beat, project.settings.bpm);
            TempoMapConverter::seconds_per_beat_from_bpm(bpm.0)
        } else {
            0.5 // fallback 120 BPM
        }
    }

    /// Get BPM at a given beat. Checks live external tempo first.
    /// Port of C# PlaybackEngine.GetBpmAtBeat (lines 1393-1401).
    pub fn get_bpm_at_beat(&mut self, beat: Beats) -> Bpm {
        if let Some((live_bpm, _)) = self.try_get_live_external_tempo() {
            return Bpm(live_bpm);
        }
        if let Some(project) = &mut self.project {
            project
                .tempo_map
                .get_bpm_at_beat(beat, project.settings.bpm)
        } else {
            Bpm::DEFAULT
        }
    }

    pub fn get_timeline_fallback_bpm(&self) -> f32 {
        self.project.as_ref().map_or(120.0, |p| p.settings.bpm.0)
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
        let clock_authority = self
            .project
            .as_ref()
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
        let live_clip_manager = self.live_clip_manager.as_mut().unwrap()
            as *mut crate::live_clip_manager::LiveClipManager;

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

    /// Evaluate live audio triggers against the latest audio snapshot, fire any
    /// one-shot clips, and expire one-shots whose length has elapsed. Returns
    /// true if the live-slot set changed this tick (caller marks dirty + sync).
    ///
    /// Called from `tick_playing` step 3b — BEFORE `sync_clips_to_time`
    /// (step 4) and BEFORE modulation (step 7), NOT after. Reads the audio
    /// snapshot the content thread set before this tick, same as every
    /// other evaluator this tick. Stopped-transport triggering is
    /// intentionally not handled here (one-shot expiry is beat-based and
    /// the clock is frozen when stopped) — see `tick_non_playing`'s
    /// meter-only conditioning walk for what DOES run while stopped.
    fn tick_audio_triggers(&mut self, realtime_now: f64, dt: Seconds) -> bool {
        if self.project.is_none() || self.live_clip_manager.is_none() {
            return false;
        }

        // 1. Pure decision — which configs fired this tick (immutable reads).
        //    P2: clip triggers are layer-owned now; `has_active_clip_triggers`
        //    replaces the old `setup.sends[].has_active_triggers()` gate.
        let fires = {
            let project = self.project.as_ref().unwrap();
            if project.has_active_clip_triggers() {
                self.live_trigger_state.evaluate(
                    &self.audio_snapshot,
                    &project.audio_setup,
                    &project.timeline.layers,
                    dt,
                    &mut self.fire_meters,
                )
            } else {
                Vec::new()
            }
        };

        // 2. Expire elapsed one-shots (no host needed; stop renderers here).
        let current_beat = self.current_beat as f32;
        let expired = self
            .live_clip_manager
            .as_mut()
            .unwrap()
            .expire_due_oneshots(current_beat);
        for (_, clip_id) in &expired {
            PlaybackEngine::stop_clip(self, clip_id);
            self.stopped_this_tick.push(clip_id.clone());
        }

        if fires.is_empty() {
            return !expired.is_empty();
        }

        // 3. Fire. Borrow split (mirrors `tick_midi_input`): raw pointers to
        //    project + live_clip_manager free `self` to act as &dyn LiveClipHost.
        //    SAFETY: the host methods fire_layer_oneshot calls only read state or
        //    mutate fields distinct from project / live_clip_manager.
        let project = self.project.as_mut().unwrap() as *mut manifold_core::project::Project;
        let lcm = self.live_clip_manager.as_mut().unwrap()
            as *mut crate::live_clip_manager::LiveClipManager;
        let project_ref = unsafe { &mut *project };
        let lcm_ref = unsafe { &mut *lcm };
        for req in &fires {
            // The target IS the owning layer (P2 — no more send-label
            // auto-routing); an unresolved request (the layer was deleted
            // since this tick's snapshot of `project.timeline.layers` was
            // read) simply fires nothing.
            if let Some(layer_index) = resolve_trigger_layer(project_ref, req) {
                lcm_ref.fire_layer_oneshot(
                    project_ref,
                    self,
                    layer_index,
                    req.one_shot_beats,
                    realtime_now,
                );
            }
        }
        true
    }

    // ─── Live external tempo ───

    /// Set live external tempo state (called by driver from sync controllers each frame).
    /// Port of C# PlaybackEngine.SetLiveExternalTempo (lines 543-548).
    pub fn set_live_external_tempo(&mut self, has_live: bool, bpm: Bpm, source: TempoPointSource) {
        self.live_external_tempo = if has_live {
            Some((bpm.0, source))
        } else {
            None
        };
    }

    /// Try to get live external tempo from Link or MIDI Clock.
    /// Returns the tempo regardless of clock authority — live BPM display
    /// should reflect the external source when available (Link is most accurate).
    /// Port of C# PlaybackEngine.TryGetLiveExternalTempo (lines 1404-1421),
    /// with authority gate removed so BPM readout works with any SRC setting.
    pub fn try_get_live_external_tempo(&self) -> Option<(f32, TempoPointSource)> {
        self.live_external_tempo.filter(|(bpm, _)| *bpm > 0.0)
    }

    /// Sync project settings BPM to the tempo at current beat position.
    /// Quantizes to avoid sub-step jitter dirtying the save file.
    /// Port of C# PlaybackEngine.SyncProjectBpmFromCurrentBeat (lines 1598-1620).
    pub fn sync_project_bpm_from_current_beat(&mut self) {
        let live_tempo = self.try_get_live_external_tempo();
        if let Some(project) = &mut self.project {
            let bpm_f32 = if let Some((live_bpm, _)) = live_tempo {
                live_bpm
            } else if !project.tempo_map.points().is_empty() {
                project
                    .tempo_map
                    .get_bpm_at_beat(Beats(self.current_beat), project.settings.bpm)
                    .0
            } else {
                project.settings.bpm.0
            };

            let bpm_f32 = bpm_f32.clamp(20.0, 300.0);
            let q_bpm = BeatQuantizer::quantize_bpm(bpm_f32);
            if (project.settings.bpm.0 - q_bpm).abs() > BeatQuantizer::BPM_STEP * 0.5 {
                project.settings.bpm = Bpm(q_bpm);
            }
        }
    }

    // ─── Transport helpers ───

    /// Resume all paused clips that are ready (for Play from paused/stopped).
    /// Port of C# PlaybackEngine.ResumeReadyClips (lines 1141-1155).
    pub fn resume_ready_clips(&mut self) {
        let clip_ids: Vec<ClipId> = self.active_clip_renderers.keys().cloned().collect();
        for clip_id in &clip_ids {
            if let Some(&idx) = self.active_clip_renderers.get(clip_id.as_str())
                && self.renderers[idx].needs_prepare_phase()
                && self.renderers[idx].is_clip_ready(clip_id)
                && !self.renderers[idx].is_clip_playing(clip_id)
                && !self.preparing_clips.contains(clip_id)
            {
                self.renderers[idx].resume_clip(clip_id);
            }
        }
    }

    /// Pause all active seekable clips (for transport Pause).
    /// Port of C# PlaybackEngine.PauseActiveClips (lines 1157-1168).
    pub fn pause_active_clips(&mut self) {
        let clip_ids: Vec<ClipId> = self.active_clip_renderers.keys().cloned().collect();
        for clip_id in &clip_ids {
            if let Some(&idx) = self.active_clip_renderers.get(clip_id.as_str())
                && self.renderers[idx].needs_prepare_phase()
                && self.renderers[idx].is_clip_playing(clip_id)
            {
                self.renderers[idx].pause_clip(clip_id);
            }
        }
    }

    /// Find a clip by ID — checks live slots first, then shared timeline lookup.
    /// Port of C# PlaybackEngine.FindTimelineClip (lines 1065-1074).
    pub fn find_timeline_clip(&self, clip_id: &str) -> Option<&TimelineClip> {
        if let Some(mgr) = &self.live_clip_manager
            && let Some(clip) = mgr.find_live_clip(clip_id)
        {
            return Some(clip);
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
    pub fn get_clip_start_time_seconds(&mut self, clip: &TimelineClip) -> Seconds {
        self.beat_to_timeline_time(clip.start_beat)
    }

    /// Get clip end time in timeline seconds.
    /// Port of C# PlaybackEngine.GetClipEndTimeSeconds (lines 1456-1460).
    pub fn get_clip_end_time_seconds(&mut self, clip: &TimelineClip) -> Seconds {
        self.beat_to_timeline_time(clip.end_beat())
    }

    /// Get clip duration in timeline seconds.
    /// Port of C# PlaybackEngine.GetClipDurationSeconds (lines 1463-1468).
    pub fn get_clip_duration_seconds(&mut self, clip: &TimelineClip) -> Seconds {
        let duration =
            self.get_clip_end_time_seconds(clip) - self.get_clip_start_time_seconds(clip);
        duration.max(Seconds::ZERO)
    }

    /// Get clip loop duration in timeline seconds.
    /// Port of C# PlaybackEngine.GetClipLoopDurationSeconds (lines 1471-1477).
    pub fn get_clip_loop_duration_seconds(&mut self, clip: &TimelineClip) -> f32 {
        if clip.loop_duration_beats <= Beats::ZERO {
            return 0.0;
        }
        let loop_end = self
            .beat_to_timeline_time(clip.start_beat + clip.loop_duration_beats)
            .as_f32();
        let loop_start = self.get_clip_start_time_seconds(clip).as_f32();
        (loop_end - loop_start).max(0.0)
    }

    /// Resolve the effective recorded BPM for a clip.
    /// Checks per-clip BPM first, then project recording provenance, else 0.
    /// Port of C# PlaybackEngine.ResolveClipRecordedBpm (lines 1480-1492).
    pub fn resolve_clip_recorded_bpm(&self, clip: &TimelineClip) -> f32 {
        if clip.recorded_bpm > 0.0 {
            return clip.recorded_bpm;
        }
        if let Some(project) = &self.project
            && project.recording_provenance.has_recorded_project_bpm
        {
            let bpm = project.recording_provenance.recorded_project_bpm;
            return bpm.0.clamp(20.0, 300.0);
        }
        0.0
    }

    /// Get playback rate for BPM time-stretching.
    /// Returns 1.0 for generators or clips without recorded BPM.
    /// Port of C# PlaybackEngine.GetClipPlaybackRate (lines 1495-1505).
    pub fn get_clip_playback_rate(&mut self, clip: &TimelineClip) -> f32 {
        if clip.video_clip_id.is_empty() {
            return 1.0;
        }

        let recorded_bpm = self.resolve_clip_recorded_bpm(clip);
        if recorded_bpm <= 0.0 {
            return 1.0;
        }

        let timeline_bpm = self
            .get_bpm_at_beat(Beats(self.current_beat))
            .0
            .clamp(20.0, 300.0);
        let rate = timeline_bpm / recorded_bpm;
        rate.clamp(MIN_CLIP_PLAYBACK_RATE, MAX_CLIP_PLAYBACK_RATE)
    }

    /// Try to get the recorded seconds-per-beat for a clip.
    /// Port of C# PlaybackEngine.TryGetClipRecordedSpb (lines 1508-1516).
    pub fn try_get_clip_recorded_spb(&self, clip: &TimelineClip) -> Option<f32> {
        let recorded_bpm = self.resolve_clip_recorded_bpm(clip);
        if recorded_bpm <= 0.0 {
            return None;
        }
        let spb = TempoMapConverter::seconds_per_beat_from_bpm(recorded_bpm);
        if spb > 0.0 { Some(spb) } else { None }
    }

    /// Get elapsed source-time seconds for a clip at the current playhead.
    /// Port of C# PlaybackEngine.GetClipSourceElapsedSeconds (lines 1519-1532).
    pub fn get_clip_source_elapsed_seconds(&mut self, clip: &TimelineClip) -> Seconds {
        if let Some(recorded_spb) = self.try_get_clip_recorded_spb(clip) {
            let elapsed_beats = (self.current_beat - clip.start_beat.0).max(0.0);
            return Seconds(elapsed_beats * recorded_spb as f64);
        }
        let clip_start_time = self.get_clip_start_time_seconds(clip);
        let clip_local_time = (self.current_time - clip_start_time).max(Seconds::ZERO);
        Seconds(clip_local_time.0 * self.get_clip_playback_rate(clip) as f64)
    }

    /// Get total source duration in seconds for a clip.
    /// Port of C# PlaybackEngine.GetClipSourceDurationSeconds (lines 1535-1543).
    pub fn get_clip_source_duration_seconds(&mut self, clip: &TimelineClip) -> f32 {
        if let Some(recorded_spb) = self.try_get_clip_recorded_spb(clip) {
            return (clip.duration_beats.as_f32() * recorded_spb).max(0.0);
        }
        (self.get_clip_duration_seconds(clip).as_f32() * self.get_clip_playback_rate(clip)).max(0.0)
    }

    /// Get source-time loop duration in seconds for a clip.
    /// Port of C# PlaybackEngine.GetClipSourceLoopDurationSeconds (lines 1546-1554).
    pub fn get_clip_source_loop_duration_seconds(&mut self, clip: &TimelineClip) -> f32 {
        if clip.loop_duration_beats <= Beats::ZERO {
            return 0.0;
        }
        if let Some(recorded_spb) = self.try_get_clip_recorded_spb(clip) {
            return (clip.loop_duration_beats.as_f32() * recorded_spb).max(0.0);
        }
        (self.get_clip_loop_duration_seconds(clip) * self.get_clip_playback_rate(clip)).max(0.0)
    }

    /// Compute video time for a clip (beat-domain, with looping).
    /// Uses source-elapsed and in-point. Port of C# PlaybackEngine.ComputeVideoTime
    /// (lines 1561-1581).
    pub fn compute_video_time(&mut self, clip: &TimelineClip, clip_id: &str) -> f32 {
        let source_elapsed = self.get_clip_source_elapsed_seconds(clip).as_f32();

        // Get media length from renderer if looping
        let media_length = if clip.is_looping {
            self.get_clip_media_length(clip_id)
        } else {
            0.0
        };

        let in_point = clip.in_point.as_f32();
        if clip.is_looping && media_length > 0.01 {
            let source_available = (media_length - in_point).max(0.0);
            let loop_len_sec = if clip.loop_duration_beats > Beats::ZERO {
                self.get_clip_source_loop_duration_seconds(clip)
                    .min(source_available)
            } else {
                media_length
            };

            if loop_len_sec > 0.01 {
                let wrapped =
                    source_elapsed - (source_elapsed / loop_len_sec).floor() * loop_len_sec;
                return in_point + wrapped;
            }
        }

        in_point + source_elapsed
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
        if self.preparing_clips.is_empty() {
            return;
        }

        self.became_ready_list.clear();

        let preparing_list: Vec<ClipId> = self.preparing_clips.iter().cloned().collect();
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
            let resolved_bpm = self.resolve_clip_recorded_bpm(&clip);
            let project_bpm = self
                .project
                .as_ref()
                .map(|p| p.settings.bpm.0)
                .unwrap_or(120.0);
            let media_length = self.renderers[renderer_idx].get_clip_media_length(clip_id);
            self.renderers[renderer_idx].set_clip_playback_rate(clip_id, rate);
            let video_time = self.compute_video_time(&clip, clip_id);
            self.renderers[renderer_idx].seek_clip(clip_id, video_time);
            log::info!(
                "[PlaybackEngine] Clip ready: {} | recorded_bpm={:.1} project_bpm={:.1} rate={:.4} \
                 seek={:.3}s media={:.1}s beat={:.2} start_beat={:.2}",
                clip_id,
                resolved_bpm,
                project_bpm,
                rate,
                video_time,
                media_length,
                self.current_beat,
                clip.start_beat.as_f32(),
            );

            if self.looping_clip_ids.contains(clip_id) {
                self.renderers[renderer_idx].set_clip_looping(clip_id, true);
            }

            self.renderers[renderer_idx].resume_clip(clip_id);

            // Exclude from compositor until first frame decodes
            self.recently_started_times
                .insert(clip_id.clone(), self.last_realtime_now);

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
        let looping_list: Vec<ClipId> = self.looping_clip_ids.iter().cloned().collect();
        for clip_id in &looping_list {
            let renderer_idx = match self.active_clip_renderers.get(clip_id.as_str()) {
                Some(&idx) => idx,
                None => continue,
            };

            if !self.renderers[renderer_idx].needs_prepare_phase() {
                continue;
            }
            if self.preparing_clips.contains(clip_id) {
                continue;
            }
            if !self.renderers[renderer_idx].is_clip_ready(clip_id) {
                continue;
            }

            let clip = match self.find_timeline_clip(clip_id).cloned() {
                Some(c) => c,
                None => continue,
            };
            if clip.loop_duration_beats <= Beats::ZERO {
                continue;
            }

            let in_point = clip.in_point.as_f32();
            let media_length = self.renderers[renderer_idx].get_clip_media_length(clip_id);
            let source_available = (media_length - in_point).max(0.0);
            let loop_len_sec = self
                .get_clip_source_loop_duration_seconds(&clip)
                .min(source_available);

            if loop_len_sec < 0.01 {
                continue;
            }

            let boundary = in_point + loop_len_sec;

            if self.renderers[renderer_idx].get_clip_playback_time(clip_id) >= boundary {
                self.renderers[renderer_idx].pause_clip(clip_id);
                self.renderers[renderer_idx].seek_clip(clip_id, in_point);
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
        if self.project.is_none() {
            return;
        }

        self.clips_to_stop_drift.clear();

        let active_list: Vec<(ClipId, usize)> = self
            .active_clip_renderers
            .iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect();

        for (clip_id, renderer_idx) in &active_list {
            if !self.renderers[*renderer_idx].needs_drift_correction() {
                continue;
            }
            if self.preparing_clips.contains(clip_id) {
                continue;
            }
            if !self.renderers[*renderer_idx].is_clip_ready(clip_id) {
                continue;
            }

            let clip = match self.find_timeline_clip(clip_id).cloned() {
                Some(c) => c,
                None => continue,
            };

            let rate = self.get_clip_playback_rate(&clip);
            self.renderers[*renderer_idx].set_clip_playback_rate(clip_id, rate);

            // Looping clips managed by native looping — skip drift correction
            if self.looping_clip_ids.contains(clip_id) {
                continue;
            }

            let in_point = clip.in_point.as_f32();
            let expected_video_time =
                in_point + self.get_clip_source_elapsed_seconds(&clip).as_f32();
            let out_point = in_point + self.get_clip_source_duration_seconds(&clip);

            let playback_time = self.renderers[*renderer_idx].get_clip_playback_time(clip_id);
            let media_length = self.renderers[*renderer_idx].get_clip_media_length(clip_id);

            let is_live_slot = self
                .live_clip_manager
                .as_ref()
                .is_some_and(|mgr| mgr.is_live_slot_clip(clip_id));

            // Out-point enforcement
            if !is_live_slot && playback_time >= out_point {
                self.clips_to_stop_drift.push(clip_id.clone());
                continue;
            }

            // Video reached natural end of file
            if media_length > 0.0 && playback_time >= media_length - 0.1 {
                if is_live_slot {
                    self.renderers[*renderer_idx].seek_clip(clip_id, in_point);
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
                    log_warn(&format!(
                        "[PlaybackEngine] Restarted stopped player: {clip_id}"
                    ));
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
                    log_warn(&format!(
                        "[PlaybackEngine] Drift correction: {clip_id} ({drift:.3}s)"
                    ));
                }
            }
        }

        // Stop clips that exceeded their out-point (deferred to avoid borrow conflict)
        let to_stop: Vec<ClipId> = self.clips_to_stop_drift.drain(..).collect();
        for clip_id in &to_stop {
            self.stop_clip(clip_id);
        }
    }

    /// Re-apply playback rates to all active clips.
    /// Port of C# PlaybackEngine.UpdateActiveClipPlaybackRates (lines 952-962).
    pub fn update_active_clip_playback_rates(&mut self) {
        let active_list: Vec<(ClipId, usize)> = self
            .active_clip_renderers
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
        let active_list: Vec<(ClipId, usize)> = self
            .active_clip_renderers
            .iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect();

        for (clip_id, renderer_idx) in &active_list {
            if !self.renderers[*renderer_idx].needs_prepare_phase() {
                continue;
            }
            if self.preparing_clips.contains(clip_id.as_str()) {
                continue;
            }
            if !self.renderers[*renderer_idx].is_clip_ready(clip_id) {
                continue;
            }

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
        clip_end_time_seconds: Seconds,
        is_live_clip: bool,
    ) -> bool {
        let start_time = match self.recently_started_times.get(clip_id) {
            Some(&t) => t,
            None => return false,
        };

        let mut gate_time: f64 = if is_live_clip {
            LIVE_RECENTLY_STARTED_TIME as f64
        } else {
            RECENTLY_STARTED_TIME as f64
        };
        let remaining = clip_end_time_seconds - self.current_time;
        if remaining > Seconds::ZERO {
            gate_time = gate_time.min(remaining.0 * 0.4);
        }

        (self.last_realtime_now - start_time) < gate_time
    }

    /// Filter active clips to only those ready for compositing.
    /// Applies recently-started gate for video clips.
    /// Port of C# PlaybackEngine.FilterReadyClips (lines 1193-1239).
    pub fn filter_ready_clips(&mut self, pre_render_dt: Seconds) -> Vec<ActiveClipRef> {
        // Resolve should-be-active clips (timeline + live slots).
        // Skip re-query if sync_clips_to_time already populated the scratch this frame.
        self.compositor_fallback_clips.clear();
        if self.timeline_query_frame != self.last_frame_count {
            self.query_active_timeline_clips();
        }
        self.compositor_fallback_clips
            .extend(self.timeline_active_scratch.iter().cloned());
        if let Some(mgr) = &self.live_clip_manager {
            mgr.fill_live_slot_refs(&mut self.compositor_fallback_clips);
        }

        // Pre-render all renderers (generators blit shaders, video is no-op)
        for renderer in &mut self.renderers {
            renderer.pre_render(
                self.current_time,
                Beats(self.current_beat),
                pre_render_dt.as_f32(),
            );
        }

        // Filter to ready clips (index-based to avoid borrow conflict)
        self.ready_clips_list.clear();
        for i in 0..self.compositor_fallback_clips.len() {
            let entry = &self.compositor_fallback_clips[i];
            let renderer_idx = match self.active_clip_renderers.get(entry.clip_id.as_str()) {
                Some(&idx) => idx,
                None => continue,
            };
            if !self.renderers[renderer_idx].is_clip_ready(&entry.clip_id) {
                continue;
            }

            // Skip clips whose RenderTexture hasn't had time to decode (video-specific).
            // Bypass in export mode — the gate prevents glitches during interactive
            // scrubbing but causes black frames and clip-transition gaps in export.
            if !self.is_export_mode && self.renderers[renderer_idx].needs_prepare_phase() {
                let is_live_clip = self
                    .live_clip_manager
                    .as_ref()
                    .is_some_and(|mgr| mgr.is_live_slot_clip(&entry.clip_id));
                // Inline beat_to_timeline_time to avoid &mut self borrow
                let clip_end_time = if let Some(project) = &self.project {
                    TempoMapConverter::beat_to_seconds_immut(
                        &project.tempo_map,
                        entry.end_beat(),
                        project.settings.bpm,
                    )
                } else {
                    Seconds(entry.end_beat().0 * 0.5)
                };
                if self.should_exclude_recently_started(&entry.clip_id, clip_end_time, is_live_clip)
                {
                    continue;
                }
            }

            self.ready_clips_list
                .push(self.compositor_fallback_clips[i].clone());
        }

        // Layer ordering is applied in content_pipeline.rs after layer_index is assigned.

        // Clear expired recently-started entries that passed the gate
        let last_rt = self.last_realtime_now;
        self.recently_started_times
            .retain(|_id, &mut start_time| last_rt - start_time < RECENTLY_STARTED_TIME as f64);

        // Take the buffer out instead of cloning — zero allocation.
        // The empty Vec left behind has zero capacity, but on the next tick,
        // filter_ready_clips reuses compositor_fallback_clips (already pre-allocated).
        // The ready_clips Vec is consumed by TickResult and dropped after the frame.
        std::mem::take(&mut self.ready_clips_list)
    }

    /// Compute pre-warm candidates: clips near the playhead that should have decoders started.
    /// Port of C# PlaybackEngine.ComputePrewarmCandidates (lines 1251-1330).
    pub fn compute_prewarm_candidates(
        &mut self,
        force: bool,
    ) -> Option<HashMap<String, crate::video_time::PrewarmCandidate>> {
        if self
            .project
            .as_ref()
            .is_none_or(|p| p.video_library.clips.is_empty())
        {
            return None;
        }

        if !force && self.last_realtime_now < self.next_prewarm_at {
            return None;
        }

        let in_live_burst = if let Some(mgr) = &self.live_clip_manager {
            (self.last_realtime_now - mgr.last_live_trigger_at()) <= LIVE_PREWARM_BURST_TIME as f64
        } else {
            false
        };
        self.next_prewarm_at = self.last_realtime_now
            + if in_live_burst {
                LIVE_PREWARM_INTERVAL
            } else {
                LOOKAHEAD_PREWARM_INTERVAL
            } as f64;

        let window_start = self.current_time - Seconds(LOOKAHEAD_PREWARM_BEHIND_TIME as f64);
        let window_end = self.current_time + Seconds(LOOKAHEAD_PREWARM_AHEAD_TIME as f64);

        // Collect candidate clips (use immutable beat_to_seconds to avoid &mut self borrow)
        self.prewarm_candidates.clear();
        if let Some(project) = &self.project {
            let any_solo = project.timeline.layers.iter().any(|l| l.is_solo);
            let fallback_bpm = project.settings.bpm;

            for layer in &project.timeline.layers {
                if layer.is_muted {
                    continue;
                }
                if any_solo && !layer.is_solo {
                    continue;
                }

                for clip in &layer.clips {
                    if clip.video_clip_id.is_empty() || clip.is_muted {
                        continue;
                    }
                    if clip.video_clip_id.is_empty() {
                        continue;
                    }

                    let clip_start = TempoMapConverter::beat_to_seconds_immut(
                        &project.tempo_map,
                        clip.start_beat,
                        fallback_bpm,
                    );
                    let clip_end = TempoMapConverter::beat_to_seconds_immut(
                        &project.tempo_map,
                        clip.end_beat(),
                        fallback_bpm,
                    );
                    if clip_end < window_start {
                        continue;
                    }
                    if clip_start > window_end {
                        continue;
                    }
                    self.prewarm_candidates.push(clip.clone());
                }
            }
        }

        self.prewarm_candidates.sort_unstable_by(|a, b| {
            a.start_beat
                .partial_cmp(&b.start_beat)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Build prewarm set from candidates
        let mut prewarm_set: HashMap<String, crate::video_time::PrewarmCandidate> = HashMap::new();
        if let Some(project) = &self.project {
            for clip in &self.prewarm_candidates {
                if prewarm_set.len() >= LOOKAHEAD_PREWARM_MAX_UNIQUE_CLIPS {
                    break;
                }
                if prewarm_set.contains_key(&clip.video_clip_id) {
                    continue;
                }

                if let Some(vc) = project.video_library.find_clip_by_id(&clip.video_clip_id) {
                    prewarm_set.insert(
                        clip.video_clip_id.clone(),
                        crate::video_time::PrewarmCandidate {
                            video_clip_id: vc.id.clone(),
                            file_path: vc.file_path.clone(),
                        },
                    );
                }
            }
        }

        // Change detection
        let changed = prewarm_set.len() != self.last_prewarm_ids.len()
            || prewarm_set
                .keys()
                .any(|k| !self.last_prewarm_ids.contains(k));

        if !changed {
            return None;
        }

        self.last_prewarm_ids.clear();
        self.last_prewarm_ids.extend(prewarm_set.keys().cloned());

        Some(prewarm_set)
    }
}

/// Resolve a live audio [`FireRequest`](crate::live_trigger::FireRequest) to a
/// layer index. P2: the target IS the owning layer (send-label auto-routing
/// died with the send-owned matrix); `None` only when the layer was deleted
/// between the snapshot this tick's fires were decided against and this
/// resolve — the fire is then skipped, not force-routed elsewhere.
fn resolve_trigger_layer(
    project: &manifold_core::project::Project,
    req: &crate::live_trigger::FireRequest,
) -> Option<i32> {
    project
        .timeline
        .layer_index_for_id(&req.target_layer)
        .map(|i| i as i32)
}

// ─── LiveClipHost impl for PlaybackEngine ───────────────────────────────────

use crate::live_clip_manager::LiveClipHost;

/// PlaybackEngine implements LiveClipHost so it can be passed directly to
/// ClipLauncher / LiveClipManager without a separate adapter type.
/// Port of C# PlaybackController implementing ILiveClipHost.
impl LiveClipHost for PlaybackEngine {
    fn current_beat(&self) -> Beats {
        Beats(self.current_beat)
    }
    fn current_time(&self) -> Seconds {
        self.current_time
    }
    fn is_recording(&self) -> bool {
        self.is_recording
    }
    fn is_playing(&self) -> bool {
        self.current_state == PlaybackState::Playing
    }
    fn show_debug_logs(&self) -> bool {
        self.show_debug_logs
    }

    /// BPM at the given beat. Checks live external tempo first.
    fn get_bpm_at_beat(&self, beat: Beats) -> f32 {
        if let Some((live_bpm, _)) = self.try_get_live_external_tempo() {
            return live_bpm;
        }
        if let Some(project) = &self.project {
            // Immutable scan (tempo map is kept sorted by ensure_sorted on mutation).
            let fallback = project.settings.bpm;
            let points = project.tempo_map.clone_points();
            if points.is_empty() {
                return fallback.0.clamp(20.0, 300.0);
            }
            let mut bpm = points[0].bpm;
            for point in &points {
                if point.beat <= beat {
                    bpm = point.bpm;
                } else {
                    break;
                }
            }
            bpm.0.clamp(20.0, 300.0)
        } else {
            120.0
        }
    }

    fn get_tempo_source_at_beat(&self, _beat: Beats) -> TempoPointSource {
        // Live external tempo overrides the source.
        if let Some((_, source)) = self.try_get_live_external_tempo() {
            return source;
        }
        TempoPointSource::Unknown
    }

    fn get_beat_snapped_beat(&self) -> Beats {
        if let Some(ref resolver) = self.beat_snapped_beat_resolver {
            Beats::from_f32(resolver())
        } else {
            Beats(self.current_beat)
        }
    }

    fn get_current_absolute_tick(&self) -> i32 {
        if let Some(ref resolver) = self.absolute_tick_resolver {
            resolver()
        } else {
            self.last_frame_count as i32
        }
    }

    fn stop_clip(&mut self, clip_id: &str) {
        PlaybackEngine::stop_clip(self, clip_id);
    }

    fn mark_sync_dirty(&mut self) {
        PlaybackEngine::mark_sync_dirty(self);
    }

    fn mark_compositor_dirty(&mut self) {
        let now = Seconds(self.last_realtime_now);
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

    fn beat_to_timeline_time(&self, beat: Beats) -> Seconds {
        if let Some(project) = &self.project {
            TempoMapConverter::beat_to_seconds_immut(&project.tempo_map, beat, project.settings.bpm)
        } else {
            Seconds(beat.0 * 0.5) // fallback: 120 bpm
        }
    }
}

// ─── Sync trait implementations ───
// SyncTarget (read-only) and SyncArbiterTarget (write) allow sync controllers
// to read playback state and issue gated transport commands via SyncArbiter.
// Port of Unity ISyncTarget + ISyncArbiterTarget implemented by PlaybackController.

impl crate::sync::SyncTarget for PlaybackEngine {
    fn current_state(&self) -> PlaybackState {
        self.current_state
    }
    fn current_time(&self) -> Seconds {
        self.current_time
    }
    fn is_playing(&self) -> bool {
        self.current_state == PlaybackState::Playing
    }

    fn timeline_beat_to_time(&self, beat: Beats) -> Seconds {
        if let Some(project) = &self.project {
            TempoMapConverter::beat_to_seconds_immut(&project.tempo_map, beat, project.settings.bpm)
        } else {
            Seconds(beat.0 * 0.5) // fallback: 120 bpm
        }
    }

    fn current_project(&self) -> Option<&Project> {
        self.project.as_ref()
    }
}

impl crate::sync::SyncArbiterTarget for PlaybackEngine {
    fn current_project(&self) -> Option<&Project> {
        self.project.as_ref()
    }
    fn external_time_sync(&self) -> bool {
        self.external_time_sync
    }
    fn set_external_time_sync(&mut self, value: bool) {
        self.external_time_sync = value;
    }

    fn play(&mut self) {
        self.play();
    }

    fn pause(&mut self, clear_recording: bool) {
        self.pause();
        if clear_recording {
            self.set_recording(false);
        }
    }

    fn nudge_time(&mut self, time: Seconds) {
        self.set_time(time);
    }

    fn seek(&mut self, time: Seconds) {
        self.seek_to(time);
    }
}
