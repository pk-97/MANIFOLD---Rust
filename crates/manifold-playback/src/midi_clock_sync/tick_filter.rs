//! MIDI clock as a beat at any instant (FRAME_EXACT_TRANSPORT_DESIGN D20):
//! MIDI 1.0 position rules over timestamped clock events, and a fixed
//! 0.25 Hz tick filter with a confirmed-change refit.
//!
//! Pure: times are seconds on the caller's monotonic origin; no allocation.

/// The filter's one bandwidth (D20).
pub const FILTER_BANDWIDTH_HZ: f64 = 0.25;
const TICKS_PER_BEAT: f64 = 24.0;
const MIN_BPM: f64 = 20.0;
const MAX_BPM: f64 = 300.0;
/// A tick sooner than this many periods after the previous one is not fed
/// to the filter; it still counts for position.
const EARLY_PERIODS: f64 = 0.25;
/// A tick this many periods after the previous one may head a burst; it is
/// fed only once the next tick arrives on time.
const GAP_PERIODS: f64 = 1.5;
/// Three accepted ticks in a row further off than this, on one side and
/// each further than the last, are a tempo change.
const REFIT_MISS: f64 = 0.002;
const REFIT_RUN: usize = 3;
/// No tick for this many periods: the clock is stale and gives no sample.
const STALE_PERIODS: f64 = 4.0;

/// One MIDI clock message (D20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockKind {
    Tick,
    Start,
    Continue,
    Stop,
    /// Song Position Pointer, in sixteenths.
    SongPosition(u16),
}

/// The clock's beat and tempo at one instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockSample {
    pub beat: f64,
    pub bpm: f64,
}

pub fn period_of_bpm(bpm: f64) -> f64 {
    60.0 / (TICKS_PER_BEAT * bpm)
}

fn clamp_period(period: f64) -> f64 {
    period.clamp(period_of_bpm(MAX_BPM), period_of_bpm(MIN_BPM))
}

/// Second-order DLL on tick times, in Adriaensen's form, with one fixed
/// bandwidth and a least-squares refit on a confirmed tempo change (D20).
#[derive(Debug, Clone)]
pub struct TickFilter {
    /// The filtered time of tick `k0`.
    k0: i64,
    t0: f64,
    period: f64,
    last_arrival: f64,
    /// A tick after a gap, waiting for the next tick (`tick`).
    pending: Option<(i64, f64)>,
    /// The current run of misses: (tick, arrival, error).
    misses: [(i64, f64, f64); REFIT_RUN],
    miss_count: usize,
    /// The period is Link's (D20 with the D21 guard latched): only phase
    /// is filtered.
    locked: bool,
}

impl TickFilter {
    /// The phase starts at the first tick; the period at the best tempo known.
    pub fn seeded(k: i64, at: f64, period: f64) -> Self {
        Self {
            k0: k,
            t0: at,
            period: clamp_period(if period.is_finite() && period > 0.0 { period } else { period_of_bpm(120.0) }),
            last_arrival: at,
            pending: None,
            misses: [(0, 0.0, 0.0); REFIT_RUN],
            miss_count: 0,
            locked: false,
        }
    }

    /// Link's period while the guard is latched, or the filter's own (D20).
    pub fn lock_period(&mut self, period: Option<f64>) {
        match period.filter(|p| p.is_finite() && *p > 0.0) {
            Some(p) => {
                self.period = clamp_period(p);
                self.locked = true;
            }
            None => self.locked = false,
        }
    }

    pub fn period(&self) -> f64 {
        self.period
    }

    pub fn bpm(&self) -> f64 {
        60.0 / (TICKS_PER_BEAT * self.period)
    }

    /// The filtered time of tick `k`.
    pub fn time_of(&self, k: f64) -> f64 {
        self.t0 + (k - self.k0 as f64) * self.period
    }

    /// Tick `k` arrived at `at`. A stall never feeds the filter: a tick less
    /// than a quarter period after the previous one is a burst's tail and is
    /// dropped, and a tick after a gap waits for the next tick, so a burst's
    /// head (delayed as long as the stall) is dropped with its tail. A real
    /// tempo drop is fed one tick later. Returns whether anything was fed.
    pub fn tick(&mut self, k: i64, at: f64) -> bool {
        if !at.is_finite() {
            return false;
        }
        let gap = at - self.last_arrival;
        self.last_arrival = self.last_arrival.max(at);
        if gap < EARLY_PERIODS * self.period {
            self.pending = None;
            return false;
        }
        let mut fed = false;
        if let Some((pk, pat)) = self.pending.take() {
            fed |= self.feed(pk, pat);
        }
        if gap > GAP_PERIODS * self.period {
            self.pending = Some((k, at));
            return fed;
        }
        self.feed(k, at) || fed
    }

    fn feed(&mut self, k: i64, at: f64) -> bool {
        if k <= self.k0 {
            return false;
        }
        let n = (k - self.k0) as f64;
        let predicted = self.t0 + n * self.period;
        let e = at - predicted;

        let omega = 2.0 * std::f64::consts::PI * FILTER_BANDWIDTH_HZ * self.period;
        let b = std::f64::consts::SQRT_2 * omega;
        if self.locked {
            self.t0 = predicted + b * e;
            self.k0 = k;
            self.miss_count = 0;
            return true;
        }

        if e.abs() > REFIT_MISS {
            let grows = self.miss_count > 0 && {
                let (_, _, last) = self.misses[self.miss_count - 1];
                last.signum() == e.signum() && e.abs() > last.abs()
            };
            if !grows {
                self.miss_count = 0;
            }
            self.misses[self.miss_count] = (k, at, e);
            self.miss_count += 1;
            if self.miss_count == REFIT_RUN {
                self.refit();
                return true;
            }
        } else {
            self.miss_count = 0;
        }

        let c = omega * omega;
        self.t0 = predicted + b * e;
        self.period = clamp_period(self.period + c * e / n);
        self.k0 = k;
        true
    }

    /// The tempo changed: the period by least squares over the run, the
    /// phase restarted at its last tick.
    fn refit(&mut self) {
        let run = &self.misses[..REFIT_RUN];
        let mean_k = run.iter().map(|m| m.0 as f64).sum::<f64>() / REFIT_RUN as f64;
        let mean_t = run.iter().map(|m| m.1).sum::<f64>() / REFIT_RUN as f64;
        let (mut skt, mut skk) = (0.0, 0.0);
        for &(k, t, _) in run {
            let dk = k as f64 - mean_k;
            skt += dk * (t - mean_t);
            skk += dk * dk;
        }
        if skk > 0.0 && skt > 0.0 {
            self.period = clamp_period(skt / skk);
        }
        let (k, t, _) = run[REFIT_RUN - 1];
        self.k0 = k;
        self.t0 = t;
        self.miss_count = 0;
    }
}

/// MIDI 1.0 position rules over the clock's events, and the filter (D20).
/// Start arms position 0, Continue the current position, Song Position
/// Pointer a position in sixteenths; the first tick after Start or Continue
/// lands on the armed position and each later one adds 1/24 beat. Ticks while
/// stopped never move the position.
#[derive(Debug, Clone, Default)]
pub struct ClockTracker {
    playing: bool,
    /// The tick index the next tick lands on.
    next: i64,
    /// The index of the last tick that landed while playing.
    landed: Option<i64>,
    /// The next tick restarts the filter's phase.
    seed_due: bool,
    filter: Option<TickFilter>,
    last_tick_at: f64,
    /// The last estimate, kept across seeds as the best tempo known.
    last_bpm: Option<f64>,
    /// Link's tempo while the guard is latched (D21).
    link_bpm: Option<f64>,
}

impl ClockTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// One event, in arrival order. `fallback_bpm` seeds the period when no
    /// estimate exists yet (the map's tempo).
    pub fn event(&mut self, kind: ClockKind, at: f64, fallback_bpm: f64) {
        match kind {
            ClockKind::Start => {
                self.playing = true;
                self.next = 0;
                self.seed_due = true;
            }
            ClockKind::Continue => {
                self.playing = true;
                self.seed_due = true;
            }
            ClockKind::Stop => self.playing = false,
            ClockKind::SongPosition(sixteenths) => {
                self.next = i64::from(sixteenths) * 6;
                self.seed_due = true;
            }
            ClockKind::Tick => {
                if !self.playing {
                    return;
                }
                let k = self.next;
                self.next += 1;
                self.landed = Some(k);
                self.last_tick_at = at;
                if self.seed_due || self.filter.is_none() {
                    self.seed_due = false;
                    let bpm = self.link_bpm.or(self.last_bpm).unwrap_or(fallback_bpm);
                    let mut filter = TickFilter::seeded(k, at, period_of_bpm(bpm));
                    filter.lock_period(self.link_bpm.map(period_of_bpm));
                    self.filter = Some(filter);
                } else if let Some(filter) = &mut self.filter {
                    filter.lock_period(self.link_bpm.map(period_of_bpm));
                    filter.tick(k, at);
                }
                if let Some(filter) = &self.filter {
                    self.last_bpm = Some(filter.bpm());
                }
            }
        }
    }

    /// Link's tempo while the guard is latched: the filter takes Link's
    /// period at each tick and filters phase only (D20). `None` unlocks.
    pub fn set_link_tempo(&mut self, bpm: Option<f64>) {
        self.link_bpm = bpm.filter(|b| b.is_finite() && *b > 0.0);
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// The last landed tick and its arrival.
    pub fn last_landed(&self) -> Option<(i64, f64)> {
        self.landed.map(|k| (k, self.last_tick_at))
    }

    /// The tick the last landed tick was, while the clock has played.
    pub fn landed_tick(&self) -> Option<i64> {
        self.landed
    }

    /// The tick index the next tick lands on.
    pub fn next_tick(&self) -> i64 {
        self.next
    }

    pub fn bpm(&self) -> Option<f64> {
        self.last_bpm
    }

    /// The beat at `r` from the filter (D7): none while stopped, before the
    /// first tick after a seed point, or after four periods with no tick.
    pub fn sample_at(&self, r: f64) -> Option<ClockSample> {
        let filter = self.filter.as_ref().filter(|_| self.playing && !self.seed_due)?;
        if !r.is_finite() || r - self.last_tick_at > STALE_PERIODS * filter.period() {
            return None;
        }
        let ticks = filter.k0 as f64 + (r - filter.t0) / filter.period();
        Some(ClockSample { beat: ticks / TICKS_PER_BEAT, bpm: filter.bpm() })
    }
}
