//! Output latency and per-source offsets, measured rather than set
//! (FRAME_EXACT_TRANSPORT_DESIGN D19). Every external source is read at
//! `R = deadline + L`, `L = L_present + L_display + L_extra`, and a non-Link
//! source at `R` plus the offset it showed against Link.
//!
//! Pure: seconds on the caller's monotonic origin, no allocation.

/// s: `L_present` before 120 presents, one frame at 60 fps.
pub const DEFAULT_PRESENT_DELAY: f64 = 0.017;
/// Presents before the first median.
const PRESENT_MIN: usize = 120;
/// The median's window grows to this many presents (10 s at 60 fps). Over
/// 120 alone the median wanders by more than half the adopt step under 3 ms
/// of jitter either way, so jitter alone would cross the step.
const PRESENT_WINDOW: usize = 600;
/// s between median recomputes.
const PRESENT_RECOMPUTE: f64 = 1.0;
/// s: a new median is adopted only when it moves further than this.
const PRESENT_ADOPT_STEP: f64 = 0.001;
/// s: the extra-delay override's range.
pub const EXTRA_DELAY_MAX: f64 = 0.250;

/// s: learned offsets stay inside ±this.
pub const OFFSET_LIMIT: f64 = 0.050;
/// s of steady play an offset is learned over.
pub const LEARN_WINDOW: f64 = 30.0;
const BUCKET: f64 = 0.5;
const BUCKETS: usize = (LEARN_WINDOW / BUCKET) as usize;
/// s: the window's spread (standard deviation) must be under this.
const LEARN_SPREAD: f64 = 0.002;
/// s: a learned offset replaces the current one only when this far apart.
const OFFSET_ADOPT_STEP: f64 = 0.001;
/// s: a difference this large is not steady play.
const LEARN_MAX_DIFFERENCE: f64 = 0.050;
/// s: a gap this long between observations restarts the window.
const LEARN_MAX_GAP: f64 = 0.5;

/// The sources an offset is learned for, against Link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalibratedSource {
    MidiClock = 0,
    AbletonOsc = 1,
    Timecode = 2,
}

impl CalibratedSource {
    pub const ALL: [Self; 3] = [Self::MidiClock, Self::AbletonOsc, Self::Timecode];
}

/// `presentedTime − deadline`: the median of the last presents (from 120,
/// growing to 600), recomputed once a second and adopted only when it moves
/// by more than 1 ms, so presentation jitter never reaches the beat.
#[derive(Debug, Clone)]
struct PresentDelay {
    ring: [f64; PRESENT_WINDOW],
    head: usize,
    len: usize,
    current: f64,
    last_recompute: f64,
}

impl PresentDelay {
    fn new() -> Self {
        Self { ring: [0.0; PRESENT_WINDOW], head: 0, len: 0, current: DEFAULT_PRESENT_DELAY, last_recompute: f64::NEG_INFINITY }
    }

    fn presented(&mut self, delay: f64, now: f64) {
        // A dropped frame reports 0 and lands far before its deadline.
        if !(delay.is_finite() && (0.0..1.0).contains(&delay)) {
            return;
        }
        self.ring[(self.head + self.len) % PRESENT_WINDOW] = delay;
        if self.len < PRESENT_WINDOW {
            self.len += 1;
        } else {
            self.head = (self.head + 1) % PRESENT_WINDOW;
        }
        if self.len < PRESENT_MIN || now - self.last_recompute < PRESENT_RECOMPUTE {
            return;
        }
        self.last_recompute = now;
        // Until the ring is full its entries are the first `len` slots.
        let mut sorted = self.ring;
        let sorted = &mut sorted[..self.len];
        sorted.sort_unstable_by(f64::total_cmp);
        let n = sorted.len();
        let median = 0.5 * (sorted[(n - 1) / 2] + sorted[n / 2]);
        if (median - self.current).abs() > PRESENT_ADOPT_STEP {
            log::info!("[SyncCalibration] screen delay {:.1} ms (was {:.1})", median * 1000.0, self.current * 1000.0);
            self.current = median;
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Bucket {
    n: u32,
    sum: f64,
    sum_sq: f64,
}

impl Bucket {
    fn add(&mut self, x: f64) {
        self.n += 1;
        self.sum += x;
        self.sum_sq += x * x;
    }
}

/// One source's offset against Link: the mean of `offset + difference` over
/// 30 s of steady play, kept in half-second buckets.
#[derive(Debug, Clone)]
struct OffsetLearner {
    offset: f64,
    ring: [Bucket; BUCKETS],
    head: usize,
    len: usize,
    open: Bucket,
    open_since: f64,
    last_seen: f64,
}

impl OffsetLearner {
    fn new() -> Self {
        Self {
            offset: 0.0,
            ring: [Bucket::default(); BUCKETS],
            head: 0,
            len: 0,
            open: Bucket::default(),
            open_since: f64::NAN,
            last_seen: f64::NAN,
        }
    }

    fn restart(&mut self) {
        self.len = 0;
        self.open = Bucket::default();
        self.open_since = f64::NAN;
    }

    /// `difference`: seconds the source, read at `R + offset`, is behind
    /// Link at `R`. Returns the newly adopted offset.
    fn observe(&mut self, now: f64, difference: f64, steady: bool) -> Option<f64> {
        let gap = now - self.last_seen;
        self.last_seen = now;
        if !(steady && difference.is_finite() && now.is_finite()) || difference.abs() >= LEARN_MAX_DIFFERENCE || gap > LEARN_MAX_GAP {
            self.restart();
            return None;
        }
        if self.open_since.is_nan() {
            self.open_since = now;
        } else if now - self.open_since >= BUCKET {
            self.ring[(self.head + self.len) % BUCKETS] = self.open;
            if self.len < BUCKETS {
                self.len += 1;
            } else {
                self.head = (self.head + 1) % BUCKETS;
            }
            self.open = Bucket::default();
            self.open_since = now;
        }
        self.open.add(self.offset + difference);
        if self.len < BUCKETS {
            return None;
        }
        let total = self.ring.iter().fold(Bucket::default(), |a, b| Bucket { n: a.n + b.n, sum: a.sum + b.sum, sum_sq: a.sum_sq + b.sum_sq });
        let n = f64::from(total.n.max(1));
        let mean = total.sum / n;
        let spread = (total.sum_sq / n - mean * mean).max(0.0).sqrt();
        if spread < LEARN_SPREAD && (mean - self.offset).abs() > OFFSET_ADOPT_STEP {
            self.offset = mean.clamp(-OFFSET_LIMIT, OFFSET_LIMIT);
            return Some(self.offset);
        }
        None
    }
}

/// The read instant and the source offsets (D19).
#[derive(Debug, Clone)]
pub struct SyncCalibration {
    present: PresentDelay,
    display: f64,
    extra: f64,
    learners: [OffsetLearner; 3],
}

impl Default for SyncCalibration {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncCalibration {
    pub fn new() -> Self {
        Self { present: PresentDelay::new(), display: 0.0, extra: 0.0, learners: [OffsetLearner::new(), OffsetLearner::new(), OffsetLearner::new()] }
    }

    /// `L`: from the frame's deadline to the moment it is on screen.
    pub fn read_delay(&self) -> f64 {
        self.present.current + self.display + self.extra
    }

    pub fn present_delay(&self) -> f64 {
        self.present.current
    }

    /// One presented frame: its `presentedTime − deadline`; `now` paces the
    /// recompute.
    pub fn presented(&mut self, delay: f64, now: f64) {
        self.present.presented(delay, now);
    }

    /// The output display's own delay when it reports one, else 0.
    pub fn set_display_delay(&mut self, delay: f64) {
        self.display = if delay.is_finite() { delay.clamp(0.0, EXTRA_DELAY_MAX) } else { 0.0 };
    }

    /// `MANIFOLD_ExtraOutputDelayMs`: 0–250 ms; anything else loads as 0
    /// with a warning naming the value.
    pub fn set_extra_delay_ms(&mut self, ms: f64) {
        self.extra = if ms.is_finite() && (0.0..=EXTRA_DELAY_MAX * 1000.0).contains(&ms) {
            ms / 1000.0
        } else {
            log::warn!("[SyncCalibration] MANIFOLD_ExtraOutputDelayMs {ms} is outside 0-250 ms; using 0");
            0.0
        };
    }

    pub fn extra_delay(&self) -> f64 {
        self.extra
    }

    /// The source is read at `R + offset`.
    pub fn offset(&self, source: CalibratedSource) -> f64 {
        self.learners[source as usize].offset
    }

    pub fn offsets(&self) -> [f64; 3] {
        CalibratedSource::ALL.map(|s| self.offset(s))
    }

    /// A stored offset from an earlier set, before the first tick.
    pub fn restore_offset(&mut self, source: CalibratedSource, offset: f64) {
        if offset.is_finite() {
            self.learners[source as usize].offset = offset.clamp(-OFFSET_LIMIT, OFFSET_LIMIT);
        }
    }

    /// One tick with Link and `source` both live. `difference` is
    /// `difference_seconds(...)` of the two reads; `steady` is false while a
    /// catch-up window is open. Returns the newly adopted offset.
    pub fn observe_offset(&mut self, source: CalibratedSource, now: f64, difference: f64, steady: bool) -> Option<f64> {
        let adopted = self.learners[source as usize].observe(now, difference, steady);
        if let Some(offset) = adopted {
            log::info!("[SyncCalibration] {source:?} offset against Link {:.1} ms", offset * 1000.0);
        }
        adopted
    }
}

/// Seconds the source is behind Link: Link's beat at `R` minus the source's
/// beat (read at `R + offset`), folded into one quantum around 0, since the
/// two count beats from different origins.
pub fn difference_seconds(link_beat: f64, source_beat: f64, bpm: f64, quantum: f64) -> f64 {
    if !(bpm > 0.0 && quantum > 0.0) {
        return f64::NAN;
    }
    let d = (link_beat - source_beat + 0.5 * quantum).rem_euclid(quantum) - 0.5 * quantum;
    d * 60.0 / bpm
}

#[cfg(test)]
mod tests;
