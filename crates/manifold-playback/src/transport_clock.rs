//! Transport time as an integer frame index on the song-time-0 frame grid
//! (docs/FRAME_EXACT_TRANSPORT_DESIGN.md, D1). Time is computed from the
//! index, never accumulated, so live play and export sample the same instants.
//! External sources never move it frame by frame: they steer the beat through
//! the follow segment (D6, `external_follow`) and place it only by a seek.
//!
//! Pure: no wall clock, no tempo map, no allocation. The engine owns one.

use std::sync::atomic::{AtomicBool, Ordering};

use manifold_core::Seconds;

/// A frame's beat is taken this many frames after its time, so an edge that
/// sits exactly on a frame always counts as reached on that frame (D14).
/// Matches `Driver::evaluate_frame_aligned`.
pub const BEAT_SAMPLE_OFFSET_FRAMES: f64 = 1e-7;
/// Seeks snap forward to the first frame at or after the target, less this
/// tolerance (D4). Strictly larger than the sample offset, so any beat
/// sampled on frame k, turned back into a time, snaps to k again.
pub const SNAP_TOLERANCE_FRAMES: f64 = 2e-7;

/// The index stays inside ±2^52: time is exact in `f64` there, and no index
/// sum can overflow.
const MAX_INDEX: i64 = 1 << 52;

const MIN_FPS: f32 = 1.0;
const MAX_FPS: f32 = 1000.0;
const FALLBACK_FPS: f32 = 60.0;

static WARNED_BAD_FPS: AtomicBool = AtomicBool::new(false);

/// The one fps rule for the transport clock and the content tick grid: 1 to
/// 1000 fps is accepted, anything else runs at 60 so a corrupt setting never
/// stops the transport.
pub(crate) fn sanitize_fps(fps: f32) -> f32 {
    if (MIN_FPS..=MAX_FPS).contains(&fps) {
        return fps;
    }
    if !WARNED_BAD_FPS.swap(true, Ordering::Relaxed) {
        log::warn!("[Transport] frame rate {fps} is outside {MIN_FPS}-{MAX_FPS} fps; running at {FALLBACK_FPS}");
    }
    FALLBACK_FPS
}

/// Seconds per frame. The tick grid and the clock both use this expression,
/// so their steps agree bit for bit.
#[inline]
pub(crate) fn step_for_fps(fps: f32) -> f64 {
    1.0 / sanitize_fps(fps) as f64
}

/// Whole frames to an index. NaN maps to 0; the mutators never pass one.
fn to_index(frames: f64) -> i64 {
    if frames.is_nan() {
        return 0;
    }
    frames.clamp(-(MAX_INDEX as f64), MAX_INDEX as f64) as i64
}

/// Transport time as an integer frame index on a fixed step.
/// `time(index) = index as f64 * step`, never accumulated.
#[derive(Debug, Clone)]
pub struct TransportClock {
    step: f64,
    fps: f32,
    index: i64,
    /// Seek while playing: the next advance renders `index` first (D4).
    hold_next: bool,
    /// Net frames the index moved on the last advance.
    last_advance: i64,
    /// The frame the newest placement no frame has rendered yet landed on,
    /// and the time it aimed at.
    placed: Option<(i64, Seconds)>,
    /// The sum of every tempo edit's move of the frame time.
    tempo_shift: f64,
}

impl TransportClock {
    pub fn new(fps: f32) -> Self {
        let fps = sanitize_fps(fps);
        Self {
            step: step_for_fps(fps),
            fps,
            index: 0,
            hold_next: false,
            last_advance: 0,
            placed: None,
            tempo_shift: 0.0,
        }
    }

    pub fn time(&self) -> Seconds {
        self.time_at(self.index)
    }

    /// Time of any frame on this clock's grid.
    pub fn time_at(&self, index: i64) -> Seconds {
        Seconds(index as f64 * self.step)
    }

    /// The instant this frame's beat is taken at (D14).
    pub fn beat_sample_time(&self) -> Seconds {
        Seconds(self.time().0 + BEAT_SAMPLE_OFFSET_FRAMES * self.step)
    }

    /// The same offset before the frame: the latest instant still before
    /// anything that lands on this frame, however its beat rounds.
    pub fn frame_floor_time(&self) -> Seconds {
        Seconds(self.time().0 - BEAT_SAMPLE_OFFSET_FRAMES * self.step)
    }

    pub fn index(&self) -> i64 {
        self.index
    }

    pub fn step(&self) -> Seconds {
        Seconds(self.step)
    }

    pub fn fps(&self) -> f32 {
        self.fps
    }

    /// First frame at or after `t`.
    pub fn index_at_or_after(&self, t: Seconds) -> i64 {
        to_index((t.0 / self.step - SNAP_TOLERANCE_FRAMES).ceil())
    }

    pub fn index_nearest(&self, t: Seconds) -> i64 {
        to_index((t.0 / self.step).round())
    }

    fn set_index(&mut self, index: i64) {
        self.index = index.clamp(-MAX_INDEX, MAX_INDEX);
    }

    /// Play, resume, paused seek, tempo edit: snap forward, no hold (D4, D11).
    /// A non-finite time is ignored.
    pub fn locate(&mut self, t: Seconds) {
        if t.0.is_finite() {
            self.set_index(self.index_at_or_after(t));
            self.reanchored(t);
        }
    }

    /// Seek while playing: snap forward and hold the next advance (D4).
    pub fn seek_playing(&mut self, t: Seconds) {
        if t.0.is_finite() {
            self.set_index(self.index_at_or_after(t));
            self.reanchored(t);
            self.hold_next = true;
        }
    }

    /// Tempo edit: the same beat now sits at `t`. Move to the first frame at
    /// or after it, no hold, and no jump: no placement target, and the move
    /// adds to [`Self::tempo_shift`], so physics shifts the times it keeps
    /// and continues. A placement no frame has rendered moves with it.
    pub fn relocate(&mut self, t: Seconds) {
        if t.0.is_finite() {
            let before = self.time().0;
            self.set_index(self.index_at_or_after(t));
            self.hold_next = false;
            self.last_advance = 0;
            let moved = self.time().0 - before;
            self.tempo_shift += moved;
            if let Some((index, target)) = &mut self.placed {
                *index = self.index;
                target.0 += moved;
            }
        }
    }

    /// The sum of every tempo edit's move of the frame time so far.
    pub fn tempo_shift(&self) -> Seconds {
        Seconds(self.tempo_shift)
    }

    /// External-authority locate: nearest frame, no hold (D4).
    pub fn locate_nearest(&mut self, t: Seconds) {
        if t.0.is_finite() {
            self.set_index(self.index_nearest(t));
            self.reanchored(t);
        }
    }

    /// Stop, initialize: index 0, no hold.
    pub fn reset(&mut self) {
        self.index = 0;
        self.reanchored(Seconds(0.0));
    }

    /// Every placement ends here: it clears what belonged to the old anchor
    /// and remembers the time it aimed at.
    fn reanchored(&mut self, target: Seconds) {
        self.hold_next = false;
        self.last_advance = 0;
        // Play right after a seek re-anchors on the frame the seek landed on,
        // at that frame's own time: the seek's target still stands.
        let target = match self.placed {
            Some((index, held)) if index == self.index && target == self.time() => held,
            _ => target,
        };
        self.placed = Some((self.index, target));
    }

    /// The time the newest placement (a seek, play from a cue, a locate)
    /// aimed at, until a frame renders it, however far the transport moved
    /// since. Physics starts its tick grid there after a jump, so the grid
    /// is the same at any frame rate.
    pub fn placement_target(&self) -> Option<Seconds> {
        self.placed.map(|(_, target)| target)
    }

    /// The placement target for the frame being rendered, once: later frames
    /// get none.
    pub fn take_placement_target(&mut self) -> Option<Seconds> {
        self.placed.take().map(|(_, target)| target)
    }

    /// The index the next playing advance of `frames` lands on (the seek
    /// hold included).
    pub fn index_after(&self, frames: u32) -> i64 {
        if self.hold_next { self.index } else { self.index.saturating_add(i64::from(frames)) }
    }

    /// Once per playing tick: the seek hold, then `frames`. Returns the net
    /// frames the index moved.
    pub fn advance(&mut self, frames: u32) -> i64 {
        let base = if std::mem::take(&mut self.hold_next) { 0 } else { i64::from(frames) };
        let before = self.index;
        self.set_index(before.saturating_add(base));
        self.last_advance = self.index - before;
        self.last_advance
    }

    pub fn last_advance(&self) -> i64 {
        self.last_advance
    }

    /// Keep the current time and move to the first frame at or after it on
    /// the new step, with no hold (D10).
    pub fn set_fps(&mut self, fps: f32) {
        let fps = sanitize_fps(fps);
        if fps != self.fps {
            self.retarget(fps);
        }
    }

    fn retarget(&mut self, fps: f32) {
        let t = self.time();
        self.fps = sanitize_fps(fps);
        self.step = step_for_fps(self.fps);
        self.set_index(self.index_at_or_after(t));
        self.reanchored(t);
    }

    /// Switch to the export grid at `fps`, still anchored at song time 0 (D9, D17).
    pub fn begin_export(&mut self, fps: f32) {
        self.retarget(fps);
    }

    /// Back to the live grid. The caller seeks to its restore time.
    pub fn end_export(&mut self, live_fps: f32) {
        self.retarget(live_fps);
    }

    /// Export frame `k` is grid frame `first_index + k`, set exactly (D9).
    pub fn set_export_frame(&mut self, first_index: i64, k: u32) {
        let before = self.index;
        self.set_index(first_index.saturating_add(i64::from(k)));
        self.last_advance = self.index - before;
        self.hold_next = false;
    }
}

#[cfg(test)]
mod tests;
