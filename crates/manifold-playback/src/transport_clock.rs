//! Transport time as an integer frame index on the song-time-0 frame grid
//! (docs/FRAME_EXACT_TRANSPORT_DESIGN.md, D1). Time is computed from the
//! index, never accumulated, so live play and export sample the same instants.
//! External sources correct the index by whole frames through a phase lock
//! (D6); they never set time directly.
//!
//! Pure: no wall clock, no tempo map, no allocation. The engine owns one.

use std::sync::atomic::{AtomicBool, Ordering};

use manifold_core::{Beats, Bpm, Seconds};

/// A frame's beat is taken this many frames after its time, so an edge that
/// sits exactly on a frame always counts as reached on that frame (D14).
/// Matches `Driver::evaluate_frame_aligned`.
pub const BEAT_SAMPLE_OFFSET_FRAMES: f64 = 1e-7;
/// Seeks snap forward to the first frame at or after the target, less this
/// tolerance (D4). Strictly larger than the sample offset, so any beat
/// sampled on frame k, turned back into a time, snaps to k again.
pub const SNAP_TOLERANCE_FRAMES: f64 = 2e-7;

/// Peter's half-frame rule plus ¼ frame of hysteresis (D6). Sources keep
/// their smoothed noise under ¼ frame, so one true offset can never read past
/// this on both sides.
const ACT_ABOVE_FRAMES: f64 = 0.75;
/// Consecutive observations past `ACT_ABOVE_FRAMES` before any correction.
/// It absorbs noise beyond the contract: with ±0.3 frame of exact-source
/// noise a half-frame offset corrects at most once an hour, against 18,000
/// times when one observation is enough (D6 sweep).
const PERSIST_OBSERVATIONS: u32 = 6;
/// Above this the drift is corrected in one jump. Every jump then moves at
/// least 3 frames, so a backward jump never nets a 1-frame rewind against a
/// tick's own advance.
const SLEW_LIMIT_FRAMES: f64 = 2.5;
/// A coarse sample this far from the estimate is a locate or an outlier,
/// never jitter, so it is not averaged in.
const COARSE_OUTLIER_FRAMES: f64 = 2.0;
/// Two consecutive outliers this close together are a locate.
const COARSE_AGREE_FRAMES: f64 = 1.0;
const COARSE_ALPHA: f64 = 0.1;
/// Every source seeks above 2 s, so drift past this is corrupt input. The
/// clamp keeps the jump arithmetic exact.
const MAX_DRIFT_FRAMES: f64 = 1e6;
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

/// The whole-frame correction applied on the last advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Correction {
    #[default]
    None,
    Skip,
    Hold,
    Jump(i64),
}

impl Correction {
    /// Frames the correction moved the index, on top of the tick's own advance.
    pub fn frames(self) -> i64 {
        match self {
            Correction::None => 0,
            Correction::Skip => 1,
            Correction::Hold => -1,
            Correction::Jump(n) => n,
        }
    }
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

/// Drift estimate in frames (target minus grid) and the D6 persistence rule.
#[derive(Debug, Clone, Default)]
struct PhaseLock {
    estimate: f64,
    seeded: bool,
    /// Consecutive observations with the estimate past `ACT_ABOVE_FRAMES`
    /// on the `streak_sign` side.
    streak: u32,
    streak_sign: i64,
    /// A coarse outlier waiting for a second sample to confirm a locate.
    candidate: Option<f64>,
    last: Correction,
    /// The estimate that triggered `last`, in frames; 0 when `last` is None.
    last_drift: f64,
}

impl PhaseLock {
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn observe(&mut self, drift: f64, smoothing: Smoothing) {
        if drift.is_nan() {
            return;
        }
        let drift = drift.clamp(-MAX_DRIFT_FRAMES, MAX_DRIFT_FRAMES);
        if !self.seeded {
            self.estimate = drift;
            self.seeded = true;
        } else if smoothing == Smoothing::Coarse && (drift - self.estimate).abs() > COARSE_OUTLIER_FRAMES {
            match self.candidate.take() {
                Some(previous) if (drift - previous).abs() <= COARSE_AGREE_FRAMES => self.estimate = drift,
                _ => {
                    self.candidate = Some(drift);
                    return;
                }
            }
        } else {
            self.candidate = None;
            self.estimate += smoothing.alpha() * (drift - self.estimate);
        }
        self.count_streak();
    }

    fn count_streak(&mut self) {
        let sign = if self.estimate > ACT_ABOVE_FRAMES {
            1
        } else if self.estimate < -ACT_ABOVE_FRAMES {
            -1
        } else {
            0
        };
        self.streak = match sign {
            0 => 0,
            s if s == self.streak_sign => self.streak.saturating_add(1),
            _ => 1,
        };
        self.streak_sign = sign;
    }

    /// Whole frames to correct now; 0 for none.
    fn due(&self) -> i64 {
        if self.streak < PERSIST_OBSERVATIONS {
            return 0;
        }
        if self.estimate.abs() > SLEW_LIMIT_FRAMES {
            self.estimate.round() as i64
        } else {
            self.streak_sign
        }
    }

    /// The grid moved toward the target: carry it in the estimate so the same
    /// drift is not corrected twice, and demand a fresh streak for the next.
    fn record(&mut self, correction: Correction) {
        self.last = correction;
        self.last_drift = 0.0;
        let frames = correction.frames();
        if frames != 0 {
            self.last_drift = self.estimate;
            self.estimate -= frames as f64;
            self.streak = 0;
            self.streak_sign = 0;
        }
    }
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
    lock: PhaseLock,
    /// Net frames the index moved on the last advance.
    last_advance: i64,
}

impl TransportClock {
    pub fn new(fps: f32) -> Self {
        let fps = sanitize_fps(fps);
        Self {
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
            self.reanchored();
        }
    }

    /// Seek while playing: snap forward and hold the next advance (D4).
    pub fn seek_playing(&mut self, t: Seconds) {
        if t.0.is_finite() {
            self.set_index(self.index_at_or_after(t));
            self.reanchored();
            self.hold_next = true;
        }
    }

    /// External-authority locate: nearest frame, no hold (D4).
    pub fn locate_nearest(&mut self, t: Seconds) {
        if t.0.is_finite() {
            self.set_index(self.index_nearest(t));
            self.reanchored();
        }
    }

    /// STOPGAP(frame-exact P3): a source that re-places a grid which also
    /// advances on its own (Link every tick, AbletonOSC per message), with no
    /// phase lock yet. The grid keeps its own advance unless the target is
    /// more than ½ + ¼ frame away (D6's threshold, without persistence), so
    /// sub-frame jitter never becomes a held or skipped frame. Past it, the
    /// nearest frame. Returns whether the index moved; non-finite is ignored.
    pub fn follow_external(&mut self, target: Seconds) -> bool {
        if !target.0.is_finite() || ((target.0 - self.time().0) / self.step).abs() <= ACT_ABOVE_FRAMES {
            return false;
        }
        let before = self.index;
        self.locate_nearest(target);
        self.index != before
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
        let base = if std::mem::take(&mut self.hold_next) { 0 } else { i64::from(frames) };
        let correction = match self.lock.due() {
            0 => Correction::None,
            1 => Correction::Skip,
            // A hold removes a frame from this tick's advance; the index never
            // steps backward on a slew. With nothing to remove it waits a tick
            // and the estimate is kept.
            -1 if base >= 1 => Correction::Hold,
            -1 => Correction::None,
            n => Correction::Jump(n),
        };
        self.lock.record(correction);
        let before = self.index;
        self.set_index(before.saturating_add(base).saturating_add(correction.frames()));
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
        self.reanchored();
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

    /// One external target, already projected to this frame's deadline (D7).
    /// Call after this tick's `advance`; a correction lands on a later advance.
    /// A non-finite target is ignored.
    pub fn observe_external(&mut self, target: Seconds, smoothing: Smoothing) {
        if target.0.is_finite() {
            self.lock.observe((target.0 - self.time().0) / self.step, smoothing);
        }
    }

    /// Authority change, anchor, Stop (D6).
    pub fn clear_external(&mut self) {
        self.lock.clear();
    }

    pub fn last_correction(&self) -> Correction {
        self.lock.last
    }

    /// The smoothed drift that triggered `last_correction`; zero when it is None.
    pub fn last_correction_drift(&self) -> Seconds {
        Seconds(self.lock.last_drift * self.step)
    }
}

/// The beat under CLK or Link authority: Ableton's live tempo from an anchor
/// (D8). The anchor is a time, not a frame index, so an fps change or the
/// export grid cannot move the beat. Callers pass grid times, so the beat is
/// still a function of the index.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveTempo {
    anchor_time: f64,
    anchor_beat: f64,
    bpm: f64,
}

/// Same clamp as the tempo map's conversions.
fn live_bpm(bpm: Bpm) -> f64 {
    f64::from(bpm.0.clamp(20.0, 300.0))
}

impl LiveTempo {
    pub fn new(anchor_time: Seconds, anchor_beat: Beats, bpm: Bpm) -> Self {
        Self { anchor_time: anchor_time.0, anchor_beat: anchor_beat.0, bpm: live_bpm(bpm) }
    }

    pub fn bpm(&self) -> Bpm {
        Bpm(self.bpm as f32)
    }

    pub fn beat_at(&self, t: Seconds) -> Beats {
        Beats(self.anchor_beat + (t.0 - self.anchor_time) * self.bpm / 60.0)
    }

    pub fn time_at(&self, beat: Beats) -> Seconds {
        Seconds(self.anchor_time + (beat.0 - self.anchor_beat) * 60.0 / self.bpm)
    }

    /// Re-anchor at `at` with a new tempo, keeping the beat continuous there.
    pub fn reanchor(&mut self, at: Seconds, bpm: Bpm) {
        self.anchor_beat = self.beat_at(at).0;
        self.anchor_time = at.0;
        self.bpm = live_bpm(bpm);
    }
}

#[cfg(test)]
mod tests;
