use manifold_core::clip::TimelineClip;
use std::collections::HashSet;

/// Result of a sync computation.
pub struct SyncResult {
    pub to_stop: Vec<String>,
    pub to_start: Vec<TimelineClip>,
}

/// Pure clip scheduling logic. Port of C# ClipScheduler.
pub struct ClipScheduler {
    should_be_active_ids: HashSet<String>,
    to_stop: Vec<String>,
    to_start: Vec<TimelineClip>,
}

impl ClipScheduler {
    pub fn new() -> Self {
        Self {
            should_be_active_ids: HashSet::with_capacity(32),
            to_stop: Vec::with_capacity(16),
            to_start: Vec::with_capacity(16),
        }
    }

    /// Compute which clips need to start/stop.
    ///
    /// `timeline_active_clips`: clips that should be active based on timeline position.
    /// `currently_active_ids`: IDs of clips currently active in renderers.
    /// `looping_clip_ids`: clips with IsLooping enabled (bypass min-remaining check).
    /// `min_remaining_beats`: skip clips with less than this remaining.
    pub fn compute_sync(
        &mut self,
        _current_time: f32,
        current_beat: f32,
        timeline_active_clips: &[TimelineClip],
        currently_active_ids: &HashSet<String>,
        looping_clip_ids: &HashSet<String>,
        min_remaining_beats: f32,
    ) -> SyncResult {
        self.should_be_active_ids.clear();
        self.to_stop.clear();
        self.to_start.clear();

        // Build set of should-be-active IDs
        for clip in timeline_active_clips {
            self.should_be_active_ids.insert(clip.id.clone());
        }

        // Clips to stop: active but shouldn't be
        for id in currently_active_ids {
            if !self.should_be_active_ids.contains(id) {
                self.to_stop.push(id.clone());
            }
        }

        // Clips to start: should be active but aren't.
        // Skip clips whose remaining lifetime in BEATS is too short — UNLESS looping.
        for clip in timeline_active_clips {
            if !currently_active_ids.contains(&clip.id) {
                let remaining = clip.end_beat() - current_beat;
                if remaining < min_remaining_beats && !looping_clip_ids.contains(&clip.id) {
                    continue;
                }
                self.to_start.push(clip.clone());
            }
        }

        SyncResult {
            to_stop: self.to_stop.clone(),
            to_start: self.to_start.clone(),
        }
    }
}

impl Default for ClipScheduler {
    fn default() -> Self {
        Self::new()
    }
}
