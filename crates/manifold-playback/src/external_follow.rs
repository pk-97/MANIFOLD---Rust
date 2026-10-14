//! The beat under an external authority (FRAME_EXACT_TRANSPORT_DESIGN D6, D8):
//! a follow segment `beat(t) = anchor_beat + (t − anchor_time) × rate` whose
//! rate is the source's tempo times a small, capped trim. Time never leaves
//! the grid; the beat is steered only through the rate, or relocated by a
//! seek. Two outcomes only: trim or seek.
//!
//! The loop is the second-order delay-locked loop JACK uses (F. Adriaensen,
//! "Using a DLL to filter time", 2005), with every correction going through
//! the rate instead of moving the phase, so the beat stays continuous.
//!
//! Pure: no `Instant`, no tempo map, no allocation after construction.

use manifold_core::{Beats, Bpm, Seconds};

/// One constants row of D6. `FAST` for Link and MIDI clock, `SLOW` for
/// AbletonOSC and timecode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FollowProfile {
    /// rad/s
    pub omega_n: f64,
    pub zeta: f64,
    /// s
    pub prefilter_tau: f64,
    /// Fraction of the base tempo.
    pub trim_cap: f64,
    /// Fraction of the base tempo.
    pub integral_cap: f64,
    /// s an error past the seek threshold must last before it seeks.
    pub seek_hold: f64,
}

impl FollowProfile {
    pub const FAST: Self = Self {
        omega_n: 1.0,
        zeta: 1.0,
        prefilter_tau: 0.1,
        trim_cap: 0.005,
        integral_cap: 0.001,
        seek_hold: 0.1,
    };
    pub const SLOW: Self = Self {
        omega_n: 0.2,
        zeta: 1.0,
        prefilter_tau: 0.5,
        trim_cap: 0.005,
        integral_cap: 0.0005,
        seek_hold: 0.25,
    };

    fn kp(&self) -> f64 {
        2.0 * self.zeta * self.omega_n
    }

    fn ki(&self) -> f64 {
        self.omega_n * self.omega_n
    }
}

/// s: an error past this, lasting the profile's hold, seeks.
pub const SEEK_ERROR: f64 = 0.050;
/// The base tempo moved by more than this fraction...
pub const CATCH_UP_TRIGGER: f64 = 0.02;
/// ...within this many seconds: a real tempo change.
pub const CATCH_UP_LOOKBACK: f64 = 0.25;
/// s the window stays open after the last trigger.
pub const CATCH_UP_OPEN: f64 = 1.0;
/// s: after that, kept open while the error is above this.
pub const CATCH_UP_HOLD_ERROR: f64 = 0.003;
/// s from the first trigger: the window never stays open longer.
pub const CATCH_UP_MAX: f64 = 3.0;
/// Fraction: the catch-up trim cap never exceeds this.
pub const CATCH_UP_CAP_MAX: f64 = 0.25;
/// s: the seek threshold inside a catch-up window.
pub const CATCH_UP_SEEK_ERROR: f64 = 0.150;

const MIN_BPM: f64 = 20.0;
const MAX_BPM: f64 = 300.0;

/// One per tick, from the authority's source at its read instant (D7, D19).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExternalSample {
    /// The source's beat at the read instant.
    pub beat: f64,
    /// The source's base tempo (D21).
    pub bpm: f64,
    /// The source delivered new data this tick.
    pub fresh: bool,
    /// The beat is exact at any instant (Link only).
    pub exact: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FollowEvent {
    None,
    /// The first fresh sample after a seed point set the segment.
    Seeded,
    /// An error past the seek threshold lasted the hold: relocate to `beat`.
    Seek { beat: Beats },
}

/// `beat(t) = anchor_beat + (t − anchor_time) × beats_per_second`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FollowSegment {
    anchor_time: f64,
    anchor_beat: f64,
    beats_per_second: f64,
}

impl FollowSegment {
    fn beat_at(&self, t: f64) -> f64 {
        self.anchor_beat + (t - self.anchor_time) * self.beats_per_second
    }
}

const RING: usize = 256;

/// The base tempos of the last `CATCH_UP_LOOKBACK` seconds, and the window.
/// A fixed ring: 256 entries cover 0.25 s at 1000 fps.
#[derive(Debug, Clone)]
struct CatchUp {
    ring: [(f64, f64); RING],
    head: usize,
    len: usize,
    first_trigger: f64,
    last_trigger: f64,
    /// Largest relative tempo change since the window opened.
    change: f64,
    open: bool,
}

impl CatchUp {
    fn new() -> Self {
        Self { ring: [(0.0, 0.0); RING], head: 0, len: 0, first_trigger: 0.0, last_trigger: 0.0, change: 0.0, open: false }
    }

    fn clear(&mut self) {
        self.len = 0;
        self.open = false;
        self.change = 0.0;
    }

    /// Record this sample's base tempo and update the window. `error` is the
    /// filtered phase error in seconds.
    fn update(&mut self, t: f64, bpm: f64, error: f64) {
        let mut moved = 0.0f64;
        for i in 0..self.len {
            let (at, old) = self.ring[(self.head + RING - 1 - i) % RING];
            if t - at > CATCH_UP_LOOKBACK {
                break;
            }
            moved = moved.max((bpm - old).abs() / old);
        }
        self.ring[self.head] = (t, bpm);
        self.head = (self.head + 1) % RING;
        self.len = (self.len + 1).min(RING);

        if moved > CATCH_UP_TRIGGER {
            if !self.open {
                self.first_trigger = t;
                self.change = 0.0;
            }
            self.open = true;
            self.last_trigger = t;
            self.change = self.change.max(moved);
        } else if self.open {
            let within_max = t - self.first_trigger <= CATCH_UP_MAX;
            let recent = t - self.last_trigger <= CATCH_UP_OPEN;
            self.open = within_max && (recent || error.abs() > CATCH_UP_HOLD_ERROR);
        }
    }

    fn cap(&self, base_cap: f64) -> f64 {
        if self.open { base_cap.max(self.change / 2.0).min(CATCH_UP_CAP_MAX) } else { base_cap }
    }
}

/// The engine's converter under an external authority (D8).
#[derive(Debug, Clone)]
pub struct ExternalFollower {
    segment: FollowSegment,
    profile: FollowProfile,
    base_bpm: f64,
    trim: f64,
    integral: f64,
    /// s
    filtered_error: f64,
    last_sample_time: f64,
    /// A seed point passed: the next fresh sample sets the segment.
    awaiting_seed: bool,
    /// First sample time past the seek threshold.
    over_since: Option<f64>,
    catch_up: CatchUp,
}

fn valid_bpm(bpm: f64) -> Option<f64> {
    (bpm.is_finite() && bpm > 0.0).then(|| bpm.clamp(MIN_BPM, MAX_BPM))
}

impl ExternalFollower {
    /// The segment always exists, so the converter is never undefined. A
    /// handover from Internal starts at the frame's own beat (D6): nothing
    /// moves on that frame.
    pub fn starting_at(profile: FollowProfile, t: Seconds, beat: Beats, bpm: Bpm) -> Self {
        let bpm = valid_bpm(f64::from(bpm.0)).unwrap_or(120.0);
        Self {
            segment: FollowSegment { anchor_time: t.0, anchor_beat: beat.0, beats_per_second: bpm / 60.0 },
            profile,
            base_bpm: bpm,
            trim: 0.0,
            integral: 0.0,
            filtered_error: 0.0,
            last_sample_time: t.0,
            awaiting_seed: false,
            over_since: None,
            catch_up: CatchUp::new(),
        }
    }

    pub fn beat_at(&self, t: Seconds) -> Beats {
        Beats(self.segment.beat_at(t.0))
    }

    /// The time the segment puts `beat` at. Only near now (D8): a beat more
    /// than one beat behind the last sample would extrapolate at today's rate.
    pub fn time_at(&self, beat: Beats) -> Seconds {
        debug_assert!(
            beat.0 >= self.segment.anchor_beat - 1.0,
            "beat {} converted through the follow segment anchored at {} (D8)",
            beat.0,
            self.segment.anchor_beat
        );
        Seconds(self.segment.anchor_time + (beat.0 - self.segment.anchor_beat) / self.segment.beats_per_second)
    }

    /// `(b1 − b0) / rate`: durations never go through two absolute times (D8).
    pub fn duration(&self, b0: Beats, b1: Beats) -> Seconds {
        Seconds((b1.0 - b0.0) / self.segment.beats_per_second)
    }

    pub fn base_bpm(&self) -> Bpm {
        Bpm(self.base_bpm as f32)
    }

    pub fn profile(&self) -> FollowProfile {
        self.profile
    }

    pub fn trim(&self) -> f64 {
        self.trim
    }

    /// The last measured phase error, filtered, in seconds.
    pub fn filtered_error(&self) -> f64 {
        self.filtered_error
    }

    pub fn catch_up_open(&self) -> bool {
        self.catch_up.open
    }

    pub fn awaiting_seed(&self) -> bool {
        self.awaiting_seed
    }

    /// Seed point (D6): the segment free-wheels until the next fresh sample
    /// sets it.
    pub fn reseed(&mut self) {
        self.awaiting_seed = true;
    }

    /// After a seek under external authority: the segment at `(t, beat)`, the
    /// integral kept, since the rate gap it learned is still true (D6).
    pub fn seek_to(&mut self, t: Seconds, beat: Beats, bpm: Bpm) {
        if !(t.0.is_finite() && beat.0.is_finite()) {
            return;
        }
        if let Some(bpm) = valid_bpm(f64::from(bpm.0)) {
            self.base_bpm = bpm;
        }
        self.trim = self.integral;
        self.segment =
            FollowSegment { anchor_time: t.0, anchor_beat: beat.0, beats_per_second: self.base_bpm / 60.0 * (1.0 + self.trim) };
        self.filtered_error = 0.0;
        self.last_sample_time = t.0;
        self.over_since = None;
        self.awaiting_seed = false;
        self.catch_up.clear();
    }

    /// Bumpless switch to another source (D6): the segment is kept, the
    /// integral takes the trim, the pre-filter clears.
    pub fn hand_over(&mut self, profile: FollowProfile) {
        self.profile = profile;
        self.integral = self.trim.clamp(-profile.integral_cap, profile.integral_cap);
        self.filtered_error = 0.0;
        self.over_since = None;
    }

    /// One sample, after this tick's advance; `t` is the shown frame's grid
    /// time. Non-finite input is ignored (D18).
    pub fn observe(&mut self, t: Seconds, sample: ExternalSample) -> FollowEvent {
        let t = t.0;
        let Some(bpm) = valid_bpm(sample.bpm) else { return FollowEvent::None };
        if !(t.is_finite() && sample.beat.is_finite() && sample.fresh) {
            return FollowEvent::None;
        }
        if self.awaiting_seed {
            self.segment = FollowSegment { anchor_time: t, anchor_beat: sample.beat, beats_per_second: bpm / 60.0 };
            self.base_bpm = bpm;
            self.trim = 0.0;
            self.integral = 0.0;
            self.filtered_error = 0.0;
            self.last_sample_time = t;
            self.over_since = None;
            self.awaiting_seed = false;
            self.catch_up.clear();
            return FollowEvent::Seeded;
        }
        let dt = t - self.last_sample_time;
        if dt <= 0.0 {
            return FollowEvent::None;
        }
        self.last_sample_time = t;

        // Link's beat is exact at any instant: on its tempo change take it,
        // keeping trim and integral, so no phase error is left (D6).
        let tempo_changed = (bpm - self.base_bpm).abs() > 1e-9;
        if sample.exact && tempo_changed {
            self.catch_up.update(t, bpm, 0.0);
            self.base_bpm = bpm;
            self.segment =
                FollowSegment { anchor_time: t, anchor_beat: sample.beat, beats_per_second: bpm / 60.0 * (1.0 + self.trim) };
            self.filtered_error = 0.0;
            self.over_since = None;
            return FollowEvent::None;
        }

        let b = self.segment.beat_at(t);
        self.base_bpm = bpm;
        let error = (sample.beat - b) * 60.0 / bpm;
        self.filtered_error += (1.0 - (-dt / self.profile.prefilter_tau).exp()) * (error - self.filtered_error);
        self.catch_up.update(t, bpm, self.filtered_error);

        let open = self.catch_up.open;
        let threshold = if open { CATCH_UP_SEEK_ERROR } else { SEEK_ERROR };
        if error.abs() > threshold {
            let since = *self.over_since.get_or_insert(t);
            if t - since >= self.profile.seek_hold - 1e-9 {
                self.over_since = None;
                return FollowEvent::Seek { beat: Beats(sample.beat) };
            }
        } else {
            self.over_since = None;
        }

        let cap = self.catch_up.cap(self.profile.trim_cap);
        let kp = self.profile.kp() * if open { 2.0 } else { 1.0 };
        let pushing_past_cap = self.trim.abs() >= cap && self.filtered_error.signum() == self.trim.signum();
        if !pushing_past_cap {
            let i_cap = self.profile.integral_cap;
            self.integral = (self.integral + self.profile.ki() * self.filtered_error * dt).clamp(-i_cap, i_cap);
        }
        self.trim = (kp * self.filtered_error + self.integral).clamp(-cap, cap);
        self.segment = FollowSegment { anchor_time: t, anchor_beat: b, beats_per_second: bpm / 60.0 * (1.0 + self.trim) };
        FollowEvent::None
    }
}

#[cfg(test)]
mod tests;
