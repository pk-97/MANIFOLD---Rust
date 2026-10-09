use ahash::{AHashMap, AHashSet};
use crate::clip_controls::{ClipControlFrame, ClipControlSpan, ClipControlStart};
use manifold_core::ClipId;
use manifold_core::LayerId;
use manifold_core::{Beats, Seconds};

/// Lightweight clip reference for the per-frame pipeline.
///
/// Replaces cloned `TimelineClip` in the scheduler, filter, and compositor paths.
/// Contains only the fields needed for scheduling and compositing decisions.
/// Full `TimelineClip` data is resolved lazily via `(layer_index, clip_index)`
/// only when needed (e.g., `start_clip` — rare, per-event, not per-frame).
///
/// Clone cost: ID reference-count increments plus scalar field copies.
/// vs. TimelineClip clone: ~200+ bytes + heap allocations for legacy Vec fields.
#[derive(Debug, Clone)]
pub struct ActiveClipRef {
    /// Clip identifier (Arc<str> — clone is atomic ref-count bump).
    pub clip_id: ClipId,
    /// Index into `timeline.layers`. Used for layer descriptor lookup.
    pub layer_index: i32,
    /// Index into `layer.clips`. `u32::MAX` for live slots (not in project timeline).
    pub clip_index: u32,
    /// Beat at which this clip starts.
    pub start_beat: Beats,
    /// Earliest beat this interval controls modulation. Resuming arrangement
    /// advances this boundary without changing the clip's phase origin.
    pub control_from: Beats,
    /// Duration of this clip in beats.
    pub duration_beats: Beats,
    /// Whether this clip loops (bypasses min-remaining check in scheduler).
    pub is_looping: bool,
    /// Whether this clip is a video clip (non-empty video_clip_id).
    /// Used for renderer dispatch without needing the full TimelineClip.
    pub is_video: bool,
    /// Whether this clip is muted. Mute is presentational (P2): the clip stays
    /// active for scheduling/modulation but contributes no pixels.
    pub is_muted: bool,
    /// The layer this clip belongs to. Part of the binding identity (P3):
    /// the reconcile restarts an active clip whose realized layer differs.
    pub layer_id: LayerId,
}

impl ActiveClipRef {
    /// Sentinel clip_index for live slots (not in the project timeline).
    pub const LIVE_SLOT: u32 = u32::MAX;
    /// Sentinel clip_index for session-grid slots (not in the project
    /// timeline; resolved from `Project::session` instead). Distinct from
    /// `LIVE_SLOT` — a parallel discriminant on the same field, following
    /// the shape `is_live_slot` already uses rather than adding an enum.
    /// See `docs/SESSION_MODE_DESIGN.md` section 4.
    pub const SESSION_SLOT: u32 = u32::MAX - 1;

    /// Computed end beat (start + duration).
    #[inline]
    pub fn end_beat(&self) -> Beats {
        self.start_beat + self.duration_beats
    }

    /// The one clip-visibility predicate: does this clip contribute pixels
    /// this frame? Anything gating on "visible content" (occlusion cutoff,
    /// idle gates, activity counts) must call this, never read `is_muted`
    /// directly. Mirrors `TimelineClip::is_visible`.
    #[inline]
    pub fn is_visible(&self) -> bool {
        !self.is_muted
    }

    /// Whether this is a live slot clip (not in the project timeline).
    #[inline]
    pub fn is_live_slot(&self) -> bool {
        self.clip_index == Self::LIVE_SLOT
    }

    /// Whether this is a session-grid slot clip (not in the project
    /// timeline's per-layer clip lane).
    #[inline]
    pub fn is_session_slot(&self) -> bool {
        self.clip_index == Self::SESSION_SLOT
    }
}

/// Result of a sync computation.
/// ALIASING CONTRACT: The Vec fields are moved out of scheduler-internal buffers.
/// They are valid until the next compute_sync call, which reclaims them.
/// Port of C# ClipScheduler.SyncResult.
pub struct SyncResult {
    /// All clips that should be playing (timeline + live slots).
    pub should_be_active: Vec<ActiveClipRef>,
    /// Clip IDs to deactivate (were active, no longer should be).
    pub to_stop: Vec<ClipId>,
    /// Clips to activate (should be active, aren't yet).
    pub to_start: Vec<ActiveClipRef>,
}

/// Pure clip scheduling logic. Port of C# ClipScheduler.
/// No platform dependencies. Zero per-frame allocations (pre-allocated collections reused).
pub struct ClipScheduler {
    should_be_active_ids: AHashSet<ClipId>,
    /// Logical source membership, independent of renderer acquisition.
    control_active: AHashMap<ClipId, ActiveClipRef>,
    control_window_ids: AHashSet<ClipId>,
    // Internal buffers — drained into SyncResult each call, reclaimed next call.
    _merged_list: Vec<ActiveClipRef>,
    _to_stop: Vec<ClipId>,
    _to_start: Vec<ActiveClipRef>,
    // Reclaimed buffers from previous SyncResult.
    reclaimed_should_be_active: Vec<ActiveClipRef>,
    reclaimed_to_stop: Vec<ClipId>,
    reclaimed_to_start: Vec<ActiveClipRef>,
}

impl ClipScheduler {
    pub fn new() -> Self {
        Self {
            should_be_active_ids: AHashSet::with_capacity(32),
            control_active: AHashMap::with_capacity(32),
            control_window_ids: AHashSet::with_capacity(32),
            _merged_list: Vec::with_capacity(64),
            _to_stop: Vec::with_capacity(16),
            _to_start: Vec::with_capacity(16),
            reclaimed_should_be_active: Vec::new(),
            reclaimed_to_stop: Vec::new(),
            reclaimed_to_start: Vec::new(),
        }
    }

    /// Record controls from the same desired membership used for media sync.
    /// A renderer retry cannot generate another source event. Rebinding an
    /// active clip to a different layer updates its source silently.
    pub(crate) fn record_controls(
        &mut self,
        result: &SyncResult,
        source_window: &[ActiveClipRef],
        from: Option<Beats>,
        current: Beats,
        controls: &mut ClipControlFrame,
    ) {
        // The source query also supplies completed clips and session iterations.
        // An observed session span can end at current even while the same clip
        // remains active; its event belongs to the current-membership walk below.
        self.control_window_ids.clear();
        for entry in source_window.iter().filter(|entry| entry.end_beat() <= current) {
            if entry.is_visible() {
                controls.record_span(entry.layer_id.clone(), ClipControlSpan {
                    clip_id: entry.clip_id.clone(),
                    start_beat: entry.start_beat,
                    control_from: entry.control_from,
                    end_beat: Some(entry.end_beat()),
                });
            }
            let is_current = self.should_be_active_ids.contains(&entry.clip_id)
                && result.should_be_active.iter().any(|active| active.clip_id == entry.clip_id
                    && active.layer_id == entry.layer_id && active.start_beat == entry.start_beat);
            let starts = if entry.is_live_slot() {
                // A complete note can arrive at the previous sync boundary.
                // Keep its membership until modulation consumes this window.
                self.control_window_ids.insert(entry.clip_id.clone());
                !self.control_active.contains_key(&entry.clip_id)
            } else {
                from.is_some_and(|from| from < entry.start_beat && entry.start_beat <= current)
            };
            if !is_current && starts && entry.start_beat >= entry.control_from {
                controls.record_start(entry.layer_id.clone(), ClipControlStart {
                    sequence: 0,
                    clip_id: entry.clip_id.clone(),
                    beat: entry.start_beat,
                    is_muted: entry.is_muted,
                });
            }
            if entry.is_live_slot() && !is_current {
                self.control_active.insert(entry.clip_id.clone(), entry.clone());
            }
        }
        self.control_active.retain(|id, _| self.should_be_active_ids.contains(id)
            || self.control_window_ids.contains(id));
        for entry in &result.should_be_active {
            if entry.is_visible() {
                controls.record_span(entry.layer_id.clone(), ClipControlSpan {
                    clip_id: entry.clip_id.clone(),
                    start_beat: entry.start_beat,
                    control_from: entry.control_from,
                    end_beat: (!entry.is_live_slot()).then(|| entry.end_beat()),
                });
            }
            let starts = match self.control_active.get(&entry.clip_id) {
                None => true,
                Some(previous) => previous.layer_id == entry.layer_id
                    && entry.is_session_slot() && previous.start_beat != entry.start_beat,
            };
            if starts && entry.start_beat >= entry.control_from {
                controls.record_start(entry.layer_id.clone(), ClipControlStart {
                    sequence: 0,
                    clip_id: entry.clip_id.clone(), beat: entry.start_beat,
                    is_muted: entry.is_muted,
                });
            }
            self.control_active.insert(entry.clip_id.clone(), entry.clone());
        }
    }

    pub(crate) fn reset_controls(&mut self) {
        self.control_active.clear();
        self.control_window_ids.clear();
    }

    /// Compute what clips should start, stop, or continue playing.
    /// Pure logic — no side effects, no platform calls.
    ///
    /// Port of C# ClipScheduler.ComputeSync (ClipScheduler.cs lines 53-125).
    ///
    /// # Parameters
    /// - `current_time`: Current playback position in seconds (reserved for future use)
    /// - `current_beat`: Current playback position in beats
    /// - `timeline_active_clips`: Clips active at currentBeat from timeline query
    /// - `live_slots`: Phantom MIDI clips keyed by layer index (NoteOff lifetime)
    /// - `session_refs`: Session-grid slot clips resolved by `SessionRuntime` for
    ///   the current beat (third reference source — an input to this sole
    ///   authority, never a parallel scheduler; see `docs/SESSION_MODE_DESIGN.md`
    ///   section 4/section 9). Already gated to "should be active now" by the caller, so —
    ///   unlike `live_slots` — no additional start-beat check is applied here.
    /// - `currently_active_ids`: IDs of clips that currently have a renderer assigned
    /// - `looping_clip_ids`: Clip IDs with IsLooping enabled (bypass min-remaining check)
    /// - `min_remaining_beats`: Don't start clips with less than this remaining
    #[allow(clippy::too_many_arguments)]
    pub fn compute_sync(
        &mut self,
        _current_time: Seconds,
        current_beat: Beats,
        timeline_active_clips: &[ActiveClipRef],
        live_slots: &[ActiveClipRef],
        session_refs: &[ActiveClipRef],
        currently_active_ids: &AHashSet<ClipId>,
        looping_clip_ids: &AHashSet<ClipId>,
        min_remaining_beats: Beats,
    ) -> SyncResult {
        // Reclaim buffers from previous result to avoid allocation.
        // Swap in pre-cleared empty vecs, get back the capacity from last call.
        let mut merged = std::mem::take(&mut self.reclaimed_should_be_active);
        let mut to_stop = std::mem::take(&mut self.reclaimed_to_stop);
        let mut to_start = std::mem::take(&mut self.reclaimed_to_start);
        merged.clear();
        to_stop.clear();
        to_start.clear();
        self.should_be_active_ids.clear();

        // Copy timeline clips to internal merged list (avoids mutating caller's cached list).
        merged.extend(timeline_active_clips.iter().cloned());

        // Merge live slots. Live slots persist until CommitLiveClip() removes them
        // (triggered by NoteOff). They must NOT expire based on EndBeat — if the
        // video is shorter than the MIDI note hold duration, the player freezes on
        // last frame but the slot stays alive so NoteOff can commit the correct
        // held duration to the timeline.
        // C# ClipScheduler.cs lines 74-83.
        for slot in live_slots {
            // Live slots are NoteOff-lifetime clips and can extend past EndBeat,
            // but they must still honor their launch boundary (StartBeat).
            if current_beat + Beats(0.0001) >= slot.start_beat {
                merged.push(slot.clone());
            }
        }

        // Merge session-grid refs. `SessionRuntime::resolve_refs` already only
        // emits a ref when the sequence has a clip covering the current local
        // beat, so no start-beat gate is needed here (unlike live slots).
        merged.extend(session_refs.iter().cloned());

        // Build lookup of what should be active.
        for entry in &merged {
            self.should_be_active_ids.insert(entry.clip_id.clone());
        }

        // Compute stops — clips that are active but shouldn't be.
        for id in currently_active_ids {
            if !self.should_be_active_ids.contains(id) {
                to_stop.push(id.clone());
            }
        }

        // Compute starts — clips that should be active but aren't.
        // Skip clips whose remaining lifetime in BEATS is too short to render.
        // Beat-domain checks stay stable when external tempo nudges BPM slightly.
        // The engine zeroes `min_remaining_beats` when stopped/paused or
        // exporting, so inspecting or encoding a clip's final frame always
        // starts it; the guard only spares warm-up churn during live playback.
        for entry in &merged {
            if !currently_active_ids.contains(&entry.clip_id) {
                let remaining = entry.end_beat() - current_beat;
                if remaining < min_remaining_beats && !looping_clip_ids.contains(&entry.clip_id) {
                    continue;
                }
                to_start.push(entry.clone());
            }
        }

        SyncResult {
            should_be_active: merged,
            to_stop,
            to_start,
        }
    }

    /// Reclaim buffers from a previous SyncResult to avoid allocation on next call.
    /// Call this after consuming the SyncResult to return ownership of the Vecs.
    pub fn reclaim(&mut self, result: SyncResult) {
        self.reclaimed_should_be_active = result.should_be_active;
        self.reclaimed_to_stop = result.to_stop;
        self.reclaimed_to_start = result.to_start;
    }
}

impl Default for ClipScheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controls_tick(
        scheduler: &mut ClipScheduler,
        controls: &mut ClipControlFrame,
        refs: &[ActiveClipRef],
        beat: Beats,
    ) {
        let result = scheduler.compute_sync(
            Seconds::ZERO, beat, refs, &[], &[], &AHashSet::new(), &AHashSet::new(), Beats(1.0),
        );
        controls.clear_spans();
        scheduler.record_controls(&result, &[], None, beat, controls);
        controls.finish();
        scheduler.reclaim(result);
    }

    #[test]
    fn source_controls_do_not_depend_on_renderer_start_or_warmup() {
        use manifold_core::params::ClipTriggerSource;
        let mut scheduler = ClipScheduler::new();
        let mut controls = ClipControlFrame::default();
        let clip = make_ref("short", 0, 0.0, 0.01);
        let owner = clip.layer_id.clone();
        controls_tick(&mut scheduler, &mut controls, std::slice::from_ref(&clip), Beats(0.005));
        assert_eq!(controls.starts(&ClipTriggerSource::OwnLayer, Some(&owner)).len(), 1);
        assert_eq!(controls.elapsed(&ClipTriggerSource::OwnLayer, Some(&owner), Beats(0.005)), Some(Beats(0.005)));
        controls.clear_starts();
        // The renderer remains absent, so another media start is attempted.
        // Source membership nevertheless remains unchanged.
        controls_tick(&mut scheduler, &mut controls, &[clip], Beats(0.006));
        assert!(controls.starts(&ClipTriggerSource::OwnLayer, Some(&owner)).is_empty());
    }

    #[test]
    fn source_controls_rebind_silently_and_restart_session_iterations() {
        use manifold_core::params::ClipTriggerSource;
        let mut scheduler = ClipScheduler::new();
        let mut controls = ClipControlFrame::default();
        let mut clip = make_ref("session", 0, 0.0, 4.0);
        clip.clip_index = ActiveClipRef::SESSION_SLOT;
        controls_tick(&mut scheduler, &mut controls, std::slice::from_ref(&clip), Beats(1.0));
        controls.clear_starts();
        clip.layer_id = LayerId::new("moved-owner");
        controls_tick(&mut scheduler, &mut controls, std::slice::from_ref(&clip), Beats(1.0));
        assert!(controls.starts(&ClipTriggerSource::OwnLayer, Some(&clip.layer_id)).is_empty());
        assert!(controls.elapsed(&ClipTriggerSource::OwnLayer, Some(&clip.layer_id), Beats(1.0)).is_some());
        clip.start_beat = Beats(4.0);
        controls_tick(&mut scheduler, &mut controls, std::slice::from_ref(&clip), Beats(4.0));
        assert_eq!(controls.starts(&ClipTriggerSource::OwnLayer, Some(&clip.layer_id)).len(), 1);
    }

    #[test]
    fn completed_live_controls_deliver_once_across_sync_cadences() {
        use manifold_core::params::ClipTriggerSource;
        for observed_live in [false, true] {
            let mut scheduler = ClipScheduler::new();
            let mut controls = ClipControlFrame::default();
            let clip = make_live_ref("short-note", 0, 1.0, 0.25);
            let owner = clip.layer_id.clone();
            if observed_live {
                controls_tick(&mut scheduler, &mut controls, std::slice::from_ref(&clip), Beats(1.0));
            }
            // NoteOn can equal the previous sync beat. NoteOff has already
            // removed the slot, but the interval survives until modulation.
            for beat in [Beats(1.5), Beats(1.75)] {
                let result = scheduler.compute_sync(
                    Seconds::ZERO, beat, &[], &[], &[],
                    &AHashSet::new(), &AHashSet::new(), Beats::ZERO,
                );
                controls.clear_spans();
                scheduler.record_controls(&result, std::slice::from_ref(&clip), Some(Beats(1.0)), beat, &mut controls);
                assert_eq!(controls.starts(&ClipTriggerSource::OwnLayer, Some(&owner)).len(), 1);
                assert_eq!(controls.elapsed(&ClipTriggerSource::OwnLayer, Some(&owner), Beats(1.125)), Some(Beats(0.125)));
                assert_eq!(controls.elapsed(&ClipTriggerSource::OwnLayer, Some(&owner), Beats(1.25)), None);
                scheduler.reclaim(result);
            }
            controls.clear_starts();
            controls_tick(&mut scheduler, &mut controls, &[], Beats(2.0));
            assert!(controls.starts(&ClipTriggerSource::OwnLayer, Some(&owner)).is_empty());
            assert!(scheduler.control_active.is_empty());
        }
    }

    #[test]
    fn completed_live_controls_preserve_retrigger_boundaries() {
        use manifold_core::params::ClipTriggerSource;
        let mut scheduler = ClipScheduler::new();
        let mut controls = ClipControlFrame::default();
        let first = make_live_ref("first-note", 0, 1.0, 0.25);
        let second = make_live_ref("second-note", 0, 1.25, 4.0);
        let owner = first.layer_id.clone();
        let result = scheduler.compute_sync(
            Seconds::ZERO, Beats(1.5), &[], &[second], &[],
            &AHashSet::new(), &AHashSet::new(), Beats::ZERO,
        );
        scheduler.record_controls(&result, &[first], Some(Beats(1.0)), Beats(1.5), &mut controls);
        controls.finish();
        let starts = controls.starts(&ClipTriggerSource::OwnLayer, Some(&owner));
        assert_eq!(starts.iter().map(|start| start.beat).collect::<Vec<_>>(), vec![Beats(1.0), Beats(1.25)]);
        assert_eq!(controls.elapsed(&ClipTriggerSource::OwnLayer, Some(&owner), Beats(1.125)), Some(Beats(0.125)));
        assert_eq!(controls.elapsed(&ClipTriggerSource::OwnLayer, Some(&owner), Beats(1.375)), Some(Beats(0.125)));
    }

    fn make_ref(id: &str, layer_index: i32, start_beat: f32, duration_beats: f32) -> ActiveClipRef {
        ActiveClipRef {
            clip_id: ClipId::new(id),
            layer_index,
            clip_index: 0,
            start_beat: Beats::from_f32(start_beat),
            control_from: Beats::from_f32(start_beat),
            duration_beats: Beats::from_f32(duration_beats),
            is_looping: false,
            is_video: false,
            is_muted: false,
            layer_id: LayerId::new(format!("layer-{layer_index}")),
        }
    }

    fn make_live_ref(
        id: &str,
        layer_index: i32,
        start_beat: f32,
        duration_beats: f32,
    ) -> ActiveClipRef {
        ActiveClipRef {
            clip_id: ClipId::new(id),
            layer_index,
            clip_index: ActiveClipRef::LIVE_SLOT,
            start_beat: Beats::from_f32(start_beat),
            control_from: Beats::from_f32(start_beat),
            duration_beats: Beats::from_f32(duration_beats),
            is_looping: false,
            is_video: false,
            is_muted: false,
            layer_id: LayerId::new(format!("layer-{layer_index}")),
        }
    }

    #[test]
    fn empty_timeline_returns_empty() {
        let mut sched = ClipScheduler::new();
        let active = AHashSet::new();
        let looping = AHashSet::new();
        let result = sched.compute_sync(
            Seconds(0.0),
            Beats(0.0),
            &[],
            &[],
            &[],
            &active,
            &looping,
            Beats(0.1),
        );
        assert!(result.should_be_active.is_empty());
        assert!(result.to_stop.is_empty());
        assert!(result.to_start.is_empty());
    }

    #[test]
    fn single_active_clip_starts() {
        let mut sched = ClipScheduler::new();
        let clip = make_ref("c1", 0, 2.0, 4.0);
        let active = AHashSet::new();
        let looping = AHashSet::new();
        let result = sched.compute_sync(
            Seconds(3.0),
            Beats(3.0),
            &[clip],
            &[],
            &[],
            &active,
            &looping,
            Beats(0.1),
        );
        assert_eq!(result.should_be_active.len(), 1);
        assert_eq!(result.to_start.len(), 1);
        assert_eq!(result.to_start[0].clip_id, "c1");
    }

    #[test]
    fn already_active_not_restarted() {
        let mut sched = ClipScheduler::new();
        let clip = make_ref("c1", 0, 2.0, 4.0);
        let mut active = AHashSet::new();
        active.insert(ClipId::new("c1"));
        let looping = AHashSet::new();
        let result = sched.compute_sync(
            Seconds(3.0),
            Beats(3.0),
            &[clip],
            &[],
            &[],
            &active,
            &looping,
            Beats(0.1),
        );
        assert_eq!(result.to_start.len(), 0);
        assert_eq!(result.to_stop.len(), 0);
    }

    #[test]
    fn clip_no_longer_active_stopped() {
        let mut sched = ClipScheduler::new();
        let mut active = AHashSet::new();
        active.insert(ClipId::new("gone"));
        let looping = AHashSet::new();
        let result = sched.compute_sync(
            Seconds(7.0),
            Beats(7.0),
            &[],
            &[],
            &[],
            &active,
            &looping,
            Beats(0.1),
        );
        assert_eq!(result.to_stop.len(), 1);
        assert_eq!(result.to_stop[0], "gone");
    }

    #[test]
    fn micro_clip_skip_short_remaining() {
        let mut sched = ClipScheduler::new();
        let clip = make_ref("short", 0, 2.0, 4.0); // ends at 6.0
        let active = AHashSet::new();
        let looping = AHashSet::new();
        // current_beat = 5.95, remaining = 0.05 < 0.1 threshold
        let result = sched.compute_sync(
            Seconds(5.95),
            Beats(5.95),
            &[clip],
            &[],
            &[],
            &active,
            &looping,
            Beats(0.1),
        );
        assert_eq!(result.to_start.len(), 0);
    }

    #[test]
    fn micro_clip_skip_bypassed_for_looping() {
        let mut sched = ClipScheduler::new();
        let clip = make_ref("loop", 0, 2.0, 4.0);
        let active = AHashSet::new();
        let mut looping = AHashSet::new();
        looping.insert(ClipId::new("loop"));
        let result = sched.compute_sync(
            Seconds(5.95),
            Beats(5.95),
            &[clip],
            &[],
            &[],
            &active,
            &looping,
            Beats(0.1),
        );
        assert_eq!(result.to_start.len(), 1);
    }

    // ─── Live slot tests ───

    #[test]
    fn live_slot_merged_when_past_start_beat() {
        let mut sched = ClipScheduler::new();
        let live_clip = make_live_ref("live1", 0, 2.0, 4.0);
        let active = AHashSet::new();
        let looping = AHashSet::new();
        // current_beat = 3.0 >= live_clip.start_beat (2.0) + 0.0001
        let result = sched.compute_sync(
            Seconds(3.0),
            Beats(3.0),
            &[],
            &[live_clip],
            &[],
            &active,
            &looping,
            Beats(0.1),
        );
        assert_eq!(result.should_be_active.len(), 1);
        assert_eq!(result.to_start.len(), 1);
        assert_eq!(result.to_start[0].clip_id, "live1");
        assert!(result.to_start[0].is_live_slot());
    }

    #[test]
    fn live_slot_excluded_before_start_beat() {
        let mut sched = ClipScheduler::new();
        let live_clip = make_live_ref("live1", 0, 5.0, 4.0);
        let active = AHashSet::new();
        let looping = AHashSet::new();
        // current_beat = 3.0 < live_clip.start_beat (5.0) - 0.0001
        let result = sched.compute_sync(
            Seconds(3.0),
            Beats(3.0),
            &[],
            &[live_clip],
            &[],
            &active,
            &looping,
            Beats(0.1),
        );
        assert_eq!(result.should_be_active.len(), 0);
        assert_eq!(result.to_start.len(), 0);
    }

    #[test]
    fn live_slot_past_end_beat_still_active() {
        let mut sched = ClipScheduler::new();
        let live_clip = make_live_ref("live1", 0, 2.0, 2.0); // ends at beat 4.0
        let active = AHashSet::new();
        let looping = AHashSet::new();
        // current_beat = 5.0 > EndBeat (4.0) — but live slots persist until NoteOff
        let result = sched.compute_sync(
            Seconds(5.0),
            Beats(5.0),
            &[],
            &[live_clip],
            &[],
            &active,
            &looping,
            Beats(0.1),
        );
        assert_eq!(result.should_be_active.len(), 1);
    }

    // ─── ActiveClipRef tests ───

    #[test]
    fn active_clip_ref_end_beat() {
        let r = make_ref("c1", 0, 2.0, 4.0);
        assert!((r.end_beat().0 - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn active_clip_ref_live_slot_sentinel() {
        let r = make_live_ref("live", 0, 0.0, 1.0);
        assert!(r.is_live_slot());
        assert_eq!(r.clip_index, ActiveClipRef::LIVE_SLOT);

        let r2 = make_ref("timeline", 0, 0.0, 1.0);
        assert!(!r2.is_live_slot());
    }

    #[test]
    fn buffer_reclamation_preserves_capacity() {
        let mut sched = ClipScheduler::new();
        let active = AHashSet::new();
        let looping = AHashSet::new();

        // First call — allocates buffers.
        let clips: Vec<ActiveClipRef> = (0..20)
            .map(|i| make_ref(&format!("c{i}"), i, 0.0, 10.0))
            .collect();
        let result = sched.compute_sync(
            Seconds(1.0),
            Beats(1.0),
            &clips,
            &[],
            &[],
            &active,
            &looping,
            Beats(0.1),
        );
        assert_eq!(result.should_be_active.len(), 20);
        assert_eq!(result.to_start.len(), 20);

        // Reclaim — capacity preserved.
        sched.reclaim(result);

        // Second call — reuses buffers, zero allocation.
        let result2 = sched.compute_sync(
            Seconds(1.0),
            Beats(1.0),
            &clips[..5],
            &[],
            &[],
            &active,
            &looping,
            Beats(0.1),
        );
        // Capacity >= 20 from first call, only 5 used.
        assert_eq!(result2.should_be_active.len(), 5);
        assert!(result2.should_be_active.capacity() >= 20);
    }
}
