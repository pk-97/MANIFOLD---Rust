use manifold_core::clip::TimelineClip;
use std::collections::HashSet;

/// Result of a sync computation.
/// ALIASING CONTRACT: The Vec fields are moved out of scheduler-internal buffers.
/// They are valid until the next compute_sync call, which reclaims them.
/// Port of C# ClipScheduler.SyncResult.
pub struct SyncResult {
    /// All clips that should be playing (timeline + live slots).
    pub should_be_active: Vec<TimelineClip>,
    /// Clip IDs to deactivate (were active, no longer should be).
    pub to_stop: Vec<String>,
    /// Clips to activate (should be active, aren't yet).
    pub to_start: Vec<TimelineClip>,
}

/// Pure clip scheduling logic. Port of C# ClipScheduler.
/// No platform dependencies. Zero per-frame allocations (pre-allocated collections reused).
pub struct ClipScheduler {
    should_be_active_ids: HashSet<String>,
    // Internal buffers — drained into SyncResult each call, reclaimed next call.
    merged_list: Vec<TimelineClip>,
    to_stop: Vec<String>,
    to_start: Vec<TimelineClip>,
    // Reclaimed buffers from previous SyncResult.
    reclaimed_should_be_active: Vec<TimelineClip>,
    reclaimed_to_stop: Vec<String>,
    reclaimed_to_start: Vec<TimelineClip>,
}

impl ClipScheduler {
    pub fn new() -> Self {
        Self {
            should_be_active_ids: HashSet::with_capacity(32),
            merged_list: Vec::with_capacity(64),
            to_stop: Vec::with_capacity(16),
            to_start: Vec::with_capacity(16),
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
        _current_time: f32,
        current_beat: f32,
        timeline_active_clips: &[TimelineClip],
        live_slots: &[(i32, TimelineClip)],
        currently_active_ids: &HashSet<String>,
        looping_clip_ids: &HashSet<String>,
        min_remaining_beats: f32,
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
        merged.extend_from_slice(timeline_active_clips);

        // Merge live slots. Live slots persist until CommitLiveClip() removes them
        // (triggered by NoteOff). They must NOT expire based on EndBeat — if the
        // video is shorter than the MIDI note hold duration, the player freezes on
        // last frame but the slot stays alive so NoteOff can commit the correct
        // held duration to the timeline.
        // C# ClipScheduler.cs lines 74-83.
        for (_layer_index, clip) in live_slots {
            // Live slots are NoteOff-lifetime clips and can extend past EndBeat,
            // but they must still honor their launch boundary (StartBeat).
            if current_beat + 0.0001 >= clip.start_beat {
                merged.push(clip.clone());
            }
        }

        // Build lookup of what should be active.
        for clip in &merged {
            self.should_be_active_ids.insert(clip.id.clone());
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
        for clip in &merged {
            if !currently_active_ids.contains(&clip.id) {
                let remaining = clip.end_beat() - current_beat;
                if remaining < min_remaining_beats && !looping_clip_ids.contains(&clip.id) {
                    continue;
                }
                to_start.push(clip.clone());
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

    fn make_clip(id: &str, start_beat: f32, duration_beats: f32) -> TimelineClip {
        TimelineClip {
            id: id.to_string(),
            start_beat,
            duration_beats,
            ..Default::default()
        }
    }

    #[test]
    fn empty_timeline_returns_empty() {
        let mut sched = ClipScheduler::new();
        let active = HashSet::new();
        let looping = HashSet::new();
        let result = sched.compute_sync(0.0, 0.0, &[], &[], &active, &looping, 0.1);
        assert!(result.should_be_active.is_empty());
        assert!(result.to_stop.is_empty());
        assert!(result.to_start.is_empty());
    }

    #[test]
    fn single_active_clip_starts() {
        let mut sched = ClipScheduler::new();
        let clip = make_clip("c1", 2.0, 4.0);
        let active = HashSet::new();
        let looping = HashSet::new();
        let result = sched.compute_sync(3.0, 3.0, &[clip.clone()], &[], &active, &looping, 0.1);
        assert_eq!(result.should_be_active.len(), 1);
        assert_eq!(result.to_start.len(), 1);
        assert_eq!(result.to_start[0].id, "c1");
    }

    #[test]
    fn already_active_not_restarted() {
        let mut sched = ClipScheduler::new();
        let clip = make_clip("c1", 2.0, 4.0);
        let mut active = HashSet::new();
        active.insert("c1".to_string());
        let looping = HashSet::new();
        let result = sched.compute_sync(3.0, 3.0, &[clip], &[], &active, &looping, 0.1);
        assert_eq!(result.to_start.len(), 0);
        assert_eq!(result.to_stop.len(), 0);
    }

    #[test]
    fn clip_no_longer_active_stopped() {
        let mut sched = ClipScheduler::new();
        let mut active = HashSet::new();
        active.insert("gone".to_string());
        let looping = HashSet::new();
        let result = sched.compute_sync(7.0, 7.0, &[], &[], &active, &looping, 0.1);
        assert_eq!(result.to_stop.len(), 1);
        assert_eq!(result.to_stop[0], "gone");
    }

    #[test]
    fn micro_clip_skip_short_remaining() {
        let mut sched = ClipScheduler::new();
        let clip = make_clip("short", 2.0, 4.0); // ends at 6.0
        let active = HashSet::new();
        let looping = HashSet::new();
        // current_beat = 5.95, remaining = 0.05 < 0.1 threshold
        let result = sched.compute_sync(5.95, 5.95, &[clip], &[], &active, &looping, 0.1);
        assert_eq!(result.to_start.len(), 0);
    }

    #[test]
    fn micro_clip_skip_bypassed_for_looping() {
        let mut sched = ClipScheduler::new();
        let clip = make_clip("loop", 2.0, 4.0);
        let active = HashSet::new();
        let mut looping = HashSet::new();
        looping.insert("loop".to_string());
        let result = sched.compute_sync(5.95, 5.95, &[clip], &[], &active, &looping, 0.1);
        assert_eq!(result.to_start.len(), 1);
    }

    // ─── Live slot tests ───

    #[test]
    fn live_slot_merged_when_past_start_beat() {
        let mut sched = ClipScheduler::new();
        let live_clip = make_clip("live1", 2.0, 4.0);
        let live_slots = vec![(0i32, live_clip)];
        let active = HashSet::new();
        let looping = HashSet::new();
        // current_beat = 3.0 >= live_clip.start_beat (2.0) + 0.0001
        let result = sched.compute_sync(3.0, 3.0, &[], &live_slots, &active, &looping, 0.1);
        assert_eq!(result.should_be_active.len(), 1);
        assert_eq!(result.to_start.len(), 1);
        assert_eq!(result.to_start[0].id, "live1");
    }

    #[test]
    fn live_slot_excluded_before_start_beat() {
        let mut sched = ClipScheduler::new();
        let live_clip = make_clip("live1", 5.0, 4.0);
        let live_slots = vec![(0i32, live_clip)];
        let active = HashSet::new();
        let looping = HashSet::new();
        // current_beat = 3.0 < live_clip.start_beat (5.0) - 0.0001
        let result = sched.compute_sync(3.0, 3.0, &[], &live_slots, &active, &looping, 0.1);
        assert_eq!(result.should_be_active.len(), 0);
        assert_eq!(result.to_start.len(), 0);
    }

    #[test]
    fn live_slot_past_end_beat_still_active() {
        let mut sched = ClipScheduler::new();
        let live_clip = make_clip("live1", 2.0, 2.0); // ends at beat 4.0
        let live_slots = vec![(0i32, live_clip)];
        let active = HashSet::new();
        let looping = HashSet::new();
        // current_beat = 5.0 > EndBeat (4.0) — but live slots persist until NoteOff
        let result = sched.compute_sync(5.0, 5.0, &[], &live_slots, &active, &looping, 0.1);
        assert_eq!(result.should_be_active.len(), 1);
    }

    #[test]
    fn live_slot_and_timeline_both_active() {
        let mut sched = ClipScheduler::new();
        let timeline_clip = make_clip("t1", 0.0, 10.0);
        let live_clip = make_clip("live1", 3.0, 4.0);
        let live_slots = vec![(1i32, live_clip)];
        let active = HashSet::new();
        let looping = HashSet::new();
        let result = sched.compute_sync(5.0, 5.0, &[timeline_clip], &live_slots, &active, &looping, 0.1);
        assert_eq!(result.should_be_active.len(), 2);
        assert_eq!(result.to_start.len(), 2);
    }

    #[test]
    fn should_be_active_contains_both_timeline_and_live() {
        let mut sched = ClipScheduler::new();
        let t1 = make_clip("t1", 0.0, 10.0);
        let t2 = make_clip("t2", 4.0, 4.0);
        let live = make_clip("live1", 1.0, 10.0);
        let live_slots = vec![(0i32, live)];
        let active = HashSet::new();
        let looping = HashSet::new();
        let result = sched.compute_sync(5.0, 5.0, &[t1, t2], &live_slots, &active, &looping, 0.1);
        assert_eq!(result.should_be_active.len(), 3);
        let ids: HashSet<String> = result.should_be_active.iter().map(|c| c.id.clone()).collect();
        assert!(ids.contains("t1"));
        assert!(ids.contains("t2"));
        assert!(ids.contains("live1"));
    }

    #[test]
    fn input_list_not_mutated() {
        let mut sched = ClipScheduler::new();
        let clips = vec![make_clip("c1", 2.0, 4.0)];
        let live_clip = make_clip("live1", 1.0, 10.0);
        let live_slots = vec![(0i32, live_clip)];
        let active = HashSet::new();
        let looping = HashSet::new();
        let original_count = clips.len();
        let _ = sched.compute_sync(3.0, 3.0, &clips, &live_slots, &active, &looping, 0.1);
        assert_eq!(clips.len(), original_count);
    }

    #[test]
    fn reclaim_reuses_buffers() {
        let mut sched = ClipScheduler::new();
        let clip = make_clip("c1", 0.0, 10.0);
        let active = HashSet::new();
        let looping = HashSet::new();

        let result = sched.compute_sync(1.0, 1.0, &[clip.clone()], &[], &active, &looping, 0.1);
        assert_eq!(result.to_start.len(), 1);

        // Reclaim and run again — should reuse buffers without new allocation
        sched.reclaim(result);
        let mut active2 = HashSet::new();
        active2.insert("c1".to_string());
        let result2 = sched.compute_sync(1.0, 1.0, &[clip], &[], &active2, &looping, 0.1);
        assert_eq!(result2.to_start.len(), 0);
        assert_eq!(result2.to_stop.len(), 0);
    }
}
