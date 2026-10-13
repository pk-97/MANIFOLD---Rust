//! Transport time as an integer frame index on the project frame grid
//! (docs/FRAME_EXACT_TRANSPORT_DESIGN.md, D1). Time is always computed from the
//! index, never accumulated, so live play and export sample the same instants.
//! External sources correct the index by whole frames through a phase lock
//! with hysteresis (D6); they never set time directly.
//!
//! Pure: no wall clock, no tempo map, no allocation. The engine owns one.

use manifold_core::{Beats, Bpm, Seconds};

/// A frame's beat is taken this many frames after its time, so an edge that
/// sits exactly on a frame always counts as reached on that frame (D14). It
/// is also the seek-snap tolerance, matching `Driver::evaluate_frame_aligned`.
pub const LATTICE_TOLERANCE_FRAMES: f64 = 1e-7;

/// Hysteresis around the half-frame rule: act above ½ + h, re-arm the reverse
/// direction only inside ½ − h (D6).
const HYSTERESIS_FRAMES: f64 = 0.25;
const ACT_ABOVE_FRAMES: f64 = 0.5 + HYSTERESIS_FRAMES;
const REARM_INSIDE_FRAMES: f64 = 0.5 - HYSTERESIS_FRAMES;
/// A reverse request this large is a real move, not flapping near the
/// threshold, so it is never refused. Without it an exact source that steps
/// across the re-arm band would be stuck off by up to two frames.
const REVERSE_ALWAYS_ABOVE_FRAMES: f64 = 1.0 + HYSTERESIS_FRAMES;
/// Above this the drift is corrected in one jump instead of a frame per tick.
const SLEW_LIMIT_FRAMES: f64 = 2.0;
const COARSE_ALPHA: f64 = 0.1;

/// Seconds per frame. The timer grid and the clock both use this expression,
/// so their steps agree bit for bit.
#[inline]
pub fn step_for_fps(fps: f32) -> f64 {
    1.0 / fps as f64
}

/// A corrupt rate must not stop the transport; the settings layer owns validation.
fn sane_fps(fps: f32) -> f32 {
    if fps.is_finite() && fps > 0.0 { fps } else { 60.0 }
}

/// The whole-frame correction applied on the last advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Correction {
    #[default]
    None,
    Skip,
    Hold,
    Jump(i64),
}

/// How an external sample enters the drift estimate (D7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Smoothing {
    /// Link: exact at the frame deadline.
    Exact,
    /// MIDI clock, timecode, AbletonOSC: averaged after projection.
    Coarse,
}

impl Smoothing {
    fn alpha(self) -> f64 {
        match self {
            Smoothing::Exact => 1.0,
            Smoothing::Coarse => COARSE_ALPHA,
        }
    }
}

/// Drift estimate in frames (target minus grid) with the D6 hysteresis.
#[derive(Debug, Clone)]
struct PhaseLock {
    estimate: f64,
    seeded: bool,
    /// Sign of the last applied correction; 0 when none since the last clear.
    last_sign: i64,
    /// The estimate has been inside ½ − h since that correction.
    rearmed: bool,
    last: Correction,
}

impl Default for PhaseLock {
    fn default() -> Self {
        Self { estimate: 0.0, seeded: false, last_sign: 0, rearmed: true, last: Correction::None }
    }
}

impl PhaseLock {
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn observe(&mut self, drift: f64, alpha: f64) {
        if !drift.is_finite() {
            return;
        }
        if self.seeded {
            self.estimate += alpha * (drift - self.estimate);
        } else {
            self.estimate = drift;
            self.seeded = true;
        }
        if self.estimate.abs() < REARM_INSIDE_FRAMES {
            self.rearmed = true;
        }
    }

    /// Whole frames to correct now; 0 for none.
    fn due(&self) -> i64 {
        if !self.seeded {
            return 0;
        }
        let e = self.estimate;
        if e.abs() > SLEW_LIMIT_FRAMES {
            return e.round() as i64;
        }
        if e.abs() <= ACT_ABOVE_FRAMES {
            return 0;
        }
        let sign = if e > 0.0 { 1 } else { -1 };
        let allowed = self.last_sign == 0
            || sign == self.last_sign
            || self.rearmed
            || e.abs() > REVERSE_ALWAYS_ABOVE_FRAMES;
        if allowed { sign } else { 0 }
    }

    /// The grid moved by `frames` toward the target: carry it in the estimate
    /// so the same drift is not corrected twice.
    fn applied(&mut self, frames: i64) {
        self.estimate -= frames as f64;
        self.last_sign = frames.signum();
        self.rearmed = self.estimate.abs() < REARM_INSIDE_FRAMES;
    }
}

/// Transport time as an integer frame index on a fixed step.
/// `time(index) = origin + index as f64 * step`, never accumulated.
#[derive(Debug, Clone)]
pub struct TransportClock {
    /// 0 live; the export origin during export (D9).
    origin: Seconds,
    step: f64,
    fps: f32,
    index: i64,
    /// Seek while playing: the next advance renders `index` first (D4).
    hold_next: bool,
    lock: PhaseLock,
    /// Net frames the index moved on the last advance.
    last_advance: i64,
}

impl TransportClock {
    pub fn new(fps: f32) -> Self {
        let fps = sane_fps(fps);
        Self {
            origin: Seconds(0.0),
            step: step_for_fps(fps),
            fps,
            index: 0,
            hold_next: false,
            lock: PhaseLock::default(),
            last_advance: 0,
        }
    }

    pub fn time(&self) -> Seconds {
        self.time_at(self.index)
    }

    /// Time of any frame on this clock's grid.
    pub fn time_at(&self, index: i64) -> Seconds {
        Seconds(self.origin.0 + index as f64 * self.step)
    }

    /// The instant this frame's beat is taken at (D14).
    pub fn beat_sample_time(&self) -> Seconds {
        Seconds(self.time().0 + LATTICE_TOLERANCE_FRAMES * self.step)
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
        ((t.0 - self.origin.0) / self.step - LATTICE_TOLERANCE_FRAMES).ceil() as i64
    }

    pub fn index_nearest(&self, t: Seconds) -> i64 {
        ((t.0 - self.origin.0) / self.step).round() as i64
    }

    /// Play, resume, paused seek, tempo edit: snap forward, no hold (D4, D11).
    pub fn locate(&mut self, t: Seconds) {
        self.index = self.index_at_or_after(t);
        self.reanchored();
    }

    /// Seek while playing: snap forward and hold the next advance (D4).
    pub fn seek_playing(&mut self, t: Seconds) {
        self.index = self.index_at_or_after(t);
        self.reanchored();
        self.hold_next = true;
    }

    /// External-authority locate: nearest frame, no hold (D4).
    pub fn locate_nearest(&mut self, t: Seconds) {
        self.index = self.index_nearest(t);
        self.reanchored();
    }

    /// Stop, initialize: index 0, no hold, lock cleared.
    pub fn reset(&mut self) {
        self.index = 0;
        self.reanchored();
    }

    fn reanchored(&mut self) {
        self.hold_next = false;
        self.lock.clear();
        self.last_advance = 0;
    }

    /// Once per playing tick: the seek hold, then `frames`, then at most one
    /// correction from the phase lock. Returns the net frames the index moved,
    /// negative only for a backward jump.
    pub fn advance(&mut self, frames: u32) -> i64 {
        let base = if self.hold_next {
            self.hold_next = false;
            0
        } else {
            i64::from(frames)
        };
        let correction = match self.lock.due() {
            0 => Correction::None,
            1 => Correction::Skip,
            // A hold removes a frame from this tick's advance; the index never
            // steps backward on a slew. With nothing to remove it waits a tick.
            -1 if base >= 1 => Correction::Hold,
            -1 => Correction::None,
            n => Correction::Jump(n),
        };
        let applied = match correction {
            Correction::None => 0,
            Correction::Skip => 1,
            Correction::Hold => -1,
            Correction::Jump(n) => n,
        };
        if applied != 0 {
            self.lock.applied(applied);
        }
        self.lock.last = correction;
        let moved = base + applied;
        self.index += moved;
        self.last_advance = moved;
        moved
    }

    pub fn last_advance(&self) -> i64 {
        self.last_advance
    }

    /// Keep the current time and move to the first frame at or after it on
    /// the new step, with no hold (D10). A live tempo segment must re-anchor
    /// after this, because its anchor frame now sits at a different time.
    pub fn set_fps(&mut self, fps: f32) {
        let fps = sane_fps(fps);
        if fps == self.fps {
            return;
        }
        self.retarget(self.origin, fps);
    }

    fn retarget(&mut self, origin: Seconds, fps: f32) {
        let t = self.time();
        self.origin = origin;
        self.fps = fps;
        self.step = step_for_fps(fps);
        self.index = self.index_at_or_after(t);
        self.reanchored();
    }

    /// Switch to the export grid (D9). `origin` is 0 for the song-time-0
    /// anchor, or the export start for the marker anchor (Q2).
    pub fn begin_export(&mut self, fps: f32, origin: Seconds) {
        self.retarget(origin, sane_fps(fps));
    }

    /// Back to the live grid at song time 0. The caller seeks to its restore time.
    pub fn end_export(&mut self, live_fps: f32) {
        self.retarget(Seconds(0.0), sane_fps(live_fps));
    }

    /// Export frame `k` is grid frame `first_index + k`, set exactly (D9).
    pub fn set_export_frame(&mut self, first_index: i64, k: u32) {
        let index = first_index + i64::from(k);
        self.last_advance = index - self.index;
        self.index = index;
        self.hold_next = false;
    }

    /// One external target, already projected to this frame's deadline (D7).
    /// Call after this tick's `advance`; a correction lands on the next advance.
    pub fn observe_external(&mut self, target: Seconds, smoothing: Smoothing) {
        let drift = (target.0 - self.time().0) / self.step;
        self.lock.observe(drift, smoothing.alpha());
    }

    /// Authority change, anchor, Stop (D6).
    pub fn clear_external(&mut self) {
        self.lock.clear();
    }

    pub fn last_correction(&self) -> Correction {
        self.lock.last
    }
}

/// The beat under CLK or Link authority: Ableton's live tempo from an anchor
/// frame (D8). Computed from the index each time, never accumulated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveTempo {
    anchor_index: i64,
    anchor_beat: f64,
    bpm: f64,
}

/// Same clamp as the tempo map's conversions.
fn live_bpm(bpm: Bpm) -> f64 {
    f64::from(bpm.0.clamp(20.0, 300.0))
}

impl LiveTempo {
    pub fn new(anchor_index: i64, anchor_beat: Beats, bpm: Bpm) -> Self {
        Self { anchor_index, anchor_beat: anchor_beat.0, bpm: live_bpm(bpm) }
    }

    pub fn bpm(&self) -> Bpm {
        Bpm(self.bpm as f32)
    }

    pub fn beat_at(&self, clock: &TransportClock, t: Seconds) -> Beats {
        let since = t.0 - clock.time_at(self.anchor_index).0;
        Beats(self.anchor_beat + since * self.bpm / 60.0)
    }

    pub fn time_at(&self, clock: &TransportClock, beat: Beats) -> Seconds {
        let beats = beat.0 - self.anchor_beat;
        Seconds(clock.time_at(self.anchor_index).0 + beats * 60.0 / self.bpm)
    }

    /// Re-anchor at `index` with a new tempo, keeping the beat continuous there.
    pub fn reanchor(&mut self, clock: &TransportClock, index: i64, bpm: Bpm) {
        let beat = self.beat_at(clock, clock.time_at(index));
        self.anchor_index = index;
        self.anchor_beat = beat.0;
        self.bpm = live_bpm(bpm);
    }
}

#[cfg(test)]
mod tests;
