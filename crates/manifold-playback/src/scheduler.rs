use ahash::AHashSet;
use manifold_core::ClipId;
use manifold_core::{Beats, Seconds};

/// Lightweight clip reference for the per-frame pipeline.
///
/// Replaces cloned `TimelineClip` in the scheduler, filter, and compositor paths.
/// Contains only the fields needed for scheduling and compositing decisions.
/// Full `TimelineClip` data is resolved lazily via `(layer_index, clip_index)`
/// only when needed (e.g., `start_clip` — rare, per-event, not per-frame).
///
/// Clone cost: 1 atomic increment (Arc<str> inside ClipId) + ~40 bytes memcpy.
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
    /// Duration of this clip in beats.
    pub duration_beats: Beats,
    /// Whether this clip loops (bypasses min-remaining check in scheduler).
    pub is_looping: bool,
    /// Whether this clip is a video clip (non-empty video_clip_id).
    /// Used for renderer dispatch without needing the full TimelineClip.
    pub is_video: bool,
}

impl ActiveClipRef {
    /// Sentinel clip_index for live slots (not in the project timeline).
    pub const LIVE_SLOT: u32 = u32::MAX;

    /// Computed end beat (start + duration).
    #[inline]
    pub fn end_beat(&self) -> Beats {
        self.start_beat + self.duration_beats
    }

    /// Whether this is a live slot clip (not in the project timeline).
    #[inline]
    pub fn is_live_slot(&self) -> bool {
        self.clip_index == Self::LIVE_SLOT
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
            _merged_list: Vec::with_capacity(64),
            _to_stop: Vec::with_capacity(16),
            _to_start: Vec::with_capacity(16),
            reclaimed_should_be_active: Vec::new(),
            reclaimed_to_stop: Vec::new(),
            reclaimed_to_start: Vec::new(),
        }
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
    /// - `currently_active_ids`: IDs of clips that currently have a renderer assigned
    /// - `looping_clip_ids`: Clip IDs with IsLooping enabled (bypass min-remaining check)
    /// - `min_remaining_beats`: Don't start clips with less than this remaining
    pub fn compute_sync(
        &mut self,
        _current_time: Seconds,
        current_beat: Beats,
        timeline_active_clips: &[ActiveClipRef],
        live_slots: &[ActiveClipRef],
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
        for entry in &merged {
            if !currently_active_ids.contains(&entry.clip_id) {
                let remaining = entry.end_beat() - current_beat;
                if remaining < min_remaining_beats
                    && !looping_clip_ids.contains(&entry.clip_id)
                {
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

    fn make_ref(id: &str, layer_index: i32, start_beat: f32, duration_beats: f32) -> ActiveClipRef {
        ActiveClipRef {
            clip_id: ClipId::new(id),
            layer_index,
            clip_index: 0,
            start_beat: Beats::from_f32(start_beat),
            duration_beats: Beats::from_f32(duration_beats),
            is_looping: false,
            is_video: false,
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
            duration_beats: Beats::from_f32(duration_beats),
            is_looping: false,
            is_video: false,
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
    fn active_clip_ref_clone_is_cheap() {
        let r = make_ref("c1", 3, 1.0, 2.0);
        let cloned = r.clone();
        assert_eq!(r.clip_id, cloned.clip_id);
        assert_eq!(r.layer_index, cloned.layer_index);
        assert_eq!(r.clip_index, cloned.clip_index);
        assert!((r.start_beat.0 - cloned.start_beat.0).abs() < f64::EPSILON);
    }

    #[test]
    fn buffer_reclamation_preserves_capacity() {
        let mut sched = ClipScheduler::new();
        let active = AHashSet::new();
        let looping = AHashSet::new();

        // First call — allocates buffers.
        let clips: Vec<ActiveClipRef> =
            (0..20).map(|i| make_ref(&format!("c{i}"), i, 0.0, 10.0)).collect();
        let result = sched.compute_sync(
            Seconds(1.0),
            Beats(1.0),
            &clips,
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
            &active,
            &looping,
            Beats(0.1),
        );
        // Capacity >= 20 from first call, only 5 used.
        assert_eq!(result2.should_be_active.len(), 5);
        assert!(result2.should_be_active.capacity() >= 20);
    }
}
