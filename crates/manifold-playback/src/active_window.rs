use manifold_core::clip::TimelineClip;
use manifold_core::project::Project;

/// Incremental time-domain clip query. Port of C# ActiveTimelineClipWindow.
/// Advances cursors forward for efficient per-frame queries.
pub struct ActiveTimelineClipWindow {
    clips_by_start: Vec<TimelineClip>,
    clips_by_end: Vec<TimelineClip>,
    active: Vec<TimelineClip>,
    start_cursor: usize,
    end_cursor: usize,
    last_beat: f32,
    initialized: bool,
}

impl ActiveTimelineClipWindow {
    pub fn new() -> Self {
        Self {
            clips_by_start: Vec::new(),
            clips_by_end: Vec::new(),
            active: Vec::new(),
            start_cursor: 0,
            end_cursor: 0,
            last_beat: -1.0,
            initialized: false,
        }
    }

    pub fn reset(&mut self) {
        self.initialized = false;
        self.active.clear();
        self.start_cursor = 0;
        self.end_cursor = 0;
        self.last_beat = -1.0;
    }

    /// Get active clips at the given beat. Returns borrowed slice.
    pub fn get_active_clips(&mut self, project: &Project, beat: f32) -> &[TimelineClip] {
        let needs_rebuild = !self.initialized
            || beat < self.last_beat
            || (beat - self.last_beat) > 32.0;

        if needs_rebuild {
            self.rebuild(project);
        }

        // Advance start cursor: add clips that have started
        while self.start_cursor < self.clips_by_start.len()
            && self.clips_by_start[self.start_cursor].start_beat <= beat
        {
            let clip = self.clips_by_start[self.start_cursor].clone();
            if clip.end_beat() > beat
                && !self.active.iter().any(|c| c.id == clip.id) {
                    self.active.push(clip);
                }
            self.start_cursor += 1;
        }

        // Remove clips that have ended
        self.active.retain(|c| c.end_beat() > beat);

        self.last_beat = beat;
        &self.active
    }

    fn rebuild(&mut self, project: &Project) {
        self.clips_by_start.clear();
        self.clips_by_end.clear();
        self.active.clear();

        for layer in &project.timeline.layers {
            if layer.is_muted { continue; }
            for clip in &layer.clips {
                if !clip.is_muted {
                    self.clips_by_start.push(clip.clone());
                    self.clips_by_end.push(clip.clone());
                }
            }
        }

        self.clips_by_start.sort_by(|a, b| a.start_beat.partial_cmp(&b.start_beat).unwrap());
        self.clips_by_end.sort_by(|a, b| a.end_beat().partial_cmp(&b.end_beat()).unwrap());

        self.start_cursor = 0;
        self.end_cursor = 0;
        self.initialized = true;
    }
}

impl Default for ActiveTimelineClipWindow {
    fn default() -> Self {
        Self::new()
    }
}
