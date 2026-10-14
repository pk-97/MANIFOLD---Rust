//! Source models and the engine driver for the P3 proofs
//! (FRAME_EXACT_TRANSPORT_DESIGN.md section 5, P3 proofs).
//!
//! Each model turns Live's true timeline into raw arrivals with D7's noise:
//! Link reads, MIDI clock bytes, AbletonOSC reports, timecode messages. A
//! reader turns arrivals into one sample per tick, and `feed` hands it to the
//! engine. The readers and `feed` are the only part that changes between the
//! red commit and the green ones; the tests assert outcomes against the
//! model's true beat at the frame's screen instant.

use std::time::{Duration, Instant};

use manifold_core::project::Project;
use manifold_core::types::TempoPointSource;
use manifold_core::{Beats, Bpm, Seconds};
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::external_follow::{ExternalSample, FollowProfile};
use manifold_playback::frame_grid::FrameGrid;
use manifold_playback::midi_clock_sync::tick_filter::{ClockKind, ClockTracker};
use manifold_playback::renderer::StubRenderer;
use manifold_playback::sync_projection::{map_sample, song_time_beat_at, timecode_seconds_at};

pub const FPS: f32 = 60.0;
pub const STEP: f64 = 1.0 / 60.0;

/// Seeded xorshift so every run is reproducible.
pub struct Rng(pub u64);

impl Rng {
    pub fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
    pub fn gauss(&mut self, sigma: f64) -> f64 {
        let u = self.unit().max(1e-12);
        let v = self.unit();
        sigma * (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}

/// Live's tempo over Live's own clock: piecewise linear through `(t, bpm)`
/// points, constant outside them. A jump is two points at one instant.
#[derive(Debug, Clone)]
pub struct Tempo {
    pts: Vec<(f64, f64)>,
}

impl Tempo {
    pub fn constant(bpm: f64) -> Self {
        Self { pts: vec![(0.0, bpm)] }
    }
    fn last(&self) -> f64 {
        self.pts.last().map_or(120.0, |p| p.1)
    }
    pub fn jump(mut self, at: f64, bpm: f64) -> Self {
        let from = self.last();
        self.pts.push((at, from));
        self.pts.push((at, bpm));
        self
    }
    pub fn ramp(mut self, at: f64, dur: f64, to: f64) -> Self {
        let from = self.last();
        self.pts.push((at, from));
        self.pts.push((at + dur, to));
        self
    }
    pub fn bpm_at(&self, t: f64) -> f64 {
        let mut bpm = self.pts[0].1;
        for w in self.pts.windows(2) {
            let ((t0, b0), (t1, b1)) = (w[0], w[1]);
            if t < t0 {
                break;
            }
            bpm = if t >= t1 || t1 == t0 { b1 } else { b0 + (b1 - b0) * (t - t0) / (t1 - t0) };
        }
        bpm
    }
    /// Beats from Live's time 0 to `t`.
    pub fn beat_at(&self, t: f64) -> f64 {
        let mut beats = 0.0;
        let mut at = 0.0f64;
        let mut bpm = self.pts[0].1;
        for w in self.pts.windows(2) {
            let ((t0, b0), (t1, b1)) = (w[0], w[1]);
            if t0 > at {
                let end = t.min(t0);
                if end > at {
                    beats += (end - at) * bpm / 60.0;
                }
                at = t0;
            }
            if t <= at {
                return beats;
            }
            if t1 > t0 {
                let end = t.min(t1);
                let b_end = b0 + (b1 - b0) * (end - t0) / (t1 - t0);
                beats += (end - t0) * (b0 + b_end) / 2.0 / 60.0;
                at = end;
            }
            bpm = b1;
            if t <= at {
                return beats;
            }
        }
        beats + (t - at) * bpm / 60.0
    }
    /// Live time of `beat`, by bisection (the curve is increasing).
    pub fn time_of_beat(&self, beat: f64) -> f64 {
        let (mut lo, mut hi) = (0.0, 1.0);
        while self.beat_at(hi) < beat {
            hi *= 2.0;
        }
        for _ in 0..80 {
            let mid = 0.5 * (lo + hi);
            if self.beat_at(mid) < beat { lo = mid } else { hi = mid }
        }
        hi
    }
}

/// Live's true timeline in wall seconds: its clock runs `gap` fast against
/// the Mac's (D7: up to ±100 ppm), and it started at wall time `start`.
/// `jumps` relocate Live's song position by a number of beats at a wall time
/// (a loop wrap or a locate); the tempo curve runs on underneath.
#[derive(Debug, Clone)]
pub struct Live {
    pub tempo: Tempo,
    pub gap: f64,
    pub start: f64,
    pub jumps: Vec<(f64, f64)>,
}

impl Live {
    pub fn new(tempo: Tempo) -> Self {
        Self { tempo, gap: 80e-6, start: 0.0, jumps: Vec::new() }
    }
    /// Relocate by `secs` of song time at the current tempo, at wall time `at`.
    pub fn relocate(mut self, at: f64, secs: f64) -> Self {
        let beats = secs * self.bpm(at) / 60.0;
        self.jumps.push((at, beats));
        self
    }
    fn live_time(&self, wall: f64) -> f64 {
        (wall - self.start) * (1.0 + self.gap)
    }
    pub fn beat(&self, wall: f64) -> f64 {
        let moved: f64 = self.jumps.iter().filter(|j| wall >= j.0).map(|j| j.1).sum();
        self.tempo.beat_at(self.live_time(wall)) + moved
    }
    pub fn bpm(&self, wall: f64) -> f64 {
        self.tempo.bpm_at(self.live_time(wall))
    }
    pub fn wall_of_beat(&self, beat: f64) -> f64 {
        self.start + self.tempo.time_of_beat(beat) / (1.0 + self.gap)
    }
}

/// One sample per tick, as the content thread hands it to the engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub beat: f64,
    pub bpm: f64,
    pub fresh: bool,
    pub exact: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Link,
    Clk,
    Osc,
    Timecode,
}

/// Reads a source at the frame's read instant `r` (wall seconds), with the
/// tick waking at `now`.
pub trait Reader {
    fn kind(&self) -> Kind;
    fn read(&mut self, r: f64, now: f64) -> Option<Sample>;
}

/// Link: Live's beat at any instant, plus ±0.2 ms timeline steps every 30 s.
pub struct LinkReader {
    pub live: Live,
    steps: Vec<f64>,
}

impl LinkReader {
    pub fn new(live: Live, seed: u64) -> Self {
        let mut rng = Rng(seed | 1);
        Self { live, steps: (0..400).map(|_| rng.range(-0.0002, 0.0002)).collect() }
    }
    pub fn beat_at(&self, r: f64) -> f64 {
        let step = self.steps[((r.max(0.0) / 30.0) as usize).min(self.steps.len() - 1)];
        self.live.beat(r + step)
    }
}

impl Reader for LinkReader {
    fn kind(&self) -> Kind {
        Kind::Link
    }
    fn read(&mut self, r: f64, _now: f64) -> Option<Sample> {
        Some(Sample { beat: self.beat_at(r), bpm: self.live.tempo.bpm_at(self.live.live_time(r)), fresh: true, exact: true })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockMsg {
    Start,
    Tick,
}

/// MIDI clock arrivals from Live: 24 per beat from the true timeline, with
/// ±1 ms uniform plus 0.3 ms Gaussian jitter, 0.5% of ticks 4 ms late, and
/// optional delivery stalls released as a burst stamped on arrival.
pub fn clock_arrivals(live: &Live, until: f64, seed: u64, stalls: &[(f64, f64)], late_by: f64) -> Vec<(f64, ClockMsg)> {
    let mut rng = Rng(seed | 1);
    let mut out = vec![(live.start - 0.001 + late_by, ClockMsg::Start)];
    let mut k = 0u64;
    loop {
        let sent = live.wall_of_beat(k as f64 / 24.0);
        if sent > until {
            break;
        }
        let mut at = sent + late_by + rng.range(-0.001, 0.001) + rng.gauss(0.0003);
        if rng.unit() < 0.005 {
            at += 0.004;
        }
        for &(from, len) in stalls {
            if at >= from && at < from + len {
                at = from + len + (k % 100) as f64 * 1e-6;
            }
        }
        // MIDI is one ordered stream: nothing arrives before what was sent
        // ahead of it, so jitter never puts a tick before the Start.
        let previous = out.last().map_or(f64::NEG_INFINITY, |m| m.0);
        out.push((at.max(previous), ClockMsg::Tick));
        k += 1;
    }
    out
}

/// MIDI clock through the real position rules and tick filter (D20): each
/// arrival goes to `ClockTracker` stamped as it arrived, and the beat is the
/// filter's at the read instant.
pub struct ClkReader {
    arrivals: Vec<(f64, ClockMsg)>,
    next: usize,
    tracker: ClockTracker,
    pub link: Option<LinkReader>,
}

impl ClkReader {
    pub fn new(arrivals: Vec<(f64, ClockMsg)>) -> Self {
        Self { arrivals, next: 0, tracker: ClockTracker::new(), link: None }
    }
    pub fn with_link(mut self, link: LinkReader) -> Self {
        self.link = Some(link);
        self
    }
}

impl Reader for ClkReader {
    fn kind(&self) -> Kind {
        Kind::Clk
    }
    fn read(&mut self, r: f64, now: f64) -> Option<Sample> {
        while self.next < self.arrivals.len() && self.arrivals[self.next].0 <= now {
            let (at, msg) = self.arrivals[self.next];
            let kind = match msg {
                ClockMsg::Start => ClockKind::Start,
                ClockMsg::Tick => ClockKind::Tick,
            };
            // The map every proof plays over is at 120 BPM.
            self.tracker.event(kind, at, 120.0);
            self.next += 1;
        }
        self.tracker.sample_at(r).map(|s| Sample { beat: s.beat, bpm: s.bpm, fresh: true, exact: false })
    }
}

/// AbletonOSC: song-time reports at 10 Hz ± 20 ms, each ±10 ms off Live's
/// position, ±1 ms network jitter; the tempo listener reports Live's tempo
/// `lag` seconds after it changes. (arrival, value) lists.
pub struct OscArrivals {
    pub song: Vec<(f64, f64)>,
    pub tempo: Vec<(f64, f64)>,
}

pub fn osc_arrivals(live: &Live, until: f64, seed: u64, lag: f64) -> OscArrivals {
    let mut rng = Rng(seed | 1);
    let rtt = 0.002;
    let mut song = Vec::new();
    let mut t = live.start + 0.05;
    while t < until {
        let skew = rng.range(-0.010, 0.010);
        song.push((t + rtt / 2.0 + rng.range(-0.001, 0.001), live.beat(t + skew)));
        t += 0.1 + rng.range(-0.020, 0.020);
    }
    let mut tempo = vec![(live.start, live.bpm(live.start))];
    let mut last = live.bpm(live.start);
    let mut t = live.start;
    while t < until {
        let bpm = live.bpm(t);
        if (bpm - last).abs() > 1e-6 {
            tempo.push((t + lag + rtt / 2.0, bpm));
            last = bpm;
        }
        t += 0.01;
    }
    tempo.sort_by(|a, b| a.0.total_cmp(&b.0));
    OscArrivals { song, tempo }
}

/// AbletonOSC as the content thread reads it: the last report with its
/// arrival stamp, projected to the read instant at the tempo in force as
/// the tempo listener reported it (D7), no one-way delay, every tick.
pub struct OscReader {
    arrivals: OscArrivals,
    next_song: usize,
    next_tempo: usize,
    /// (arrival, reported beat)
    last: Option<(f64, f64)>,
    bpm: f64,
    /// The tempo before the last change, and the change's arrival.
    before: f64,
    changed_at: f64,
}

impl OscReader {
    pub fn new(arrivals: OscArrivals) -> Self {
        Self { arrivals, next_song: 0, next_tempo: 0, last: None, bpm: 120.0, before: 120.0, changed_at: f64::NEG_INFINITY }
    }
}

impl Reader for OscReader {
    fn kind(&self) -> Kind {
        Kind::Osc
    }
    fn read(&mut self, r: f64, now: f64) -> Option<Sample> {
        while self.next_tempo < self.arrivals.tempo.len() && self.arrivals.tempo[self.next_tempo].0 <= now {
            let (at, bpm) = self.arrivals.tempo[self.next_tempo];
            if bpm != self.bpm {
                (self.before, self.changed_at) = (self.bpm, at);
            }
            self.bpm = bpm;
            self.next_tempo += 1;
        }
        while self.next_song < self.arrivals.song.len() && self.arrivals.song[self.next_song].0 <= now {
            self.last = Some(self.arrivals.song[self.next_song]);
            self.next_song += 1;
        }
        let (at, beat) = self.last?;
        let beat = song_time_beat_at(beat, r - at, self.before, self.bpm, r - self.changed_at);
        Some(Sample { beat, bpm: self.bpm, fresh: true, exact: false })
    }
}

/// Timecode messages: song seconds sent on a free-running metro of `metro`
/// seconds from `phase`, floored to 25 fps labels when `labels`, arriving up
/// to 1 ms late. (arrival, seconds) pairs.
pub struct TimecodeArrivals {
    pub messages: Vec<(f64, f64)>,
    /// The label frame's length, when the messages are frame labels.
    pub frame: Option<f64>,
}

pub fn timecode_arrivals(live: &Live, until: f64, seed: u64, metro: f64, phase: f64, labels: bool) -> TimecodeArrivals {
    let mut rng = Rng(seed | 1);
    let mut messages = Vec::new();
    let mut t = live.start + phase;
    while t < until {
        let song = live.live_time(t);
        let song = if labels { (song * 25.0).floor() / 25.0 } else { song };
        messages.push((t + rng.range(0.0, 0.001), song));
        t += metro;
    }
    TimecodeArrivals { messages, frame: labels.then_some(1.0 / 25.0) }
}

/// Timecode as the content thread reads it: the last message's seconds
/// projected from its arrival to the read instant, the half-frame bias on a
/// label, then through the project's map at the map's tempo over the next
/// frame (D7).
pub struct TimecodeReader {
    arrivals: TimecodeArrivals,
    next: usize,
    /// (arrival, seconds)
    last: Option<(f64, f64)>,
    map_bpm: f64,
}

impl TimecodeReader {
    pub fn new(arrivals: TimecodeArrivals, map_bpm: f64) -> Self {
        Self { arrivals, next: 0, last: None, map_bpm }
    }
}

impl Reader for TimecodeReader {
    fn kind(&self) -> Kind {
        Kind::Timecode
    }
    fn read(&mut self, r: f64, now: f64) -> Option<Sample> {
        let messages = &self.arrivals.messages;
        while self.next < messages.len() && messages[self.next].0 <= now {
            self.last = Some(messages[self.next]);
            self.next += 1;
        }
        let (at, seconds) = self.last?;
        let s = timecode_seconds_at(seconds, r - at, self.arrivals.frame);
        let map_bpm = self.map_bpm;
        let (beat, bpm) = map_sample(s, STEP, |s| s * map_bpm / 60.0);
        Some(Sample { beat, bpm, fresh: true, exact: false })
    }
}

/// The authority as the engine sees it.
pub fn set_authority(engine: &mut PlaybackEngine, kind: Option<Kind>) {
    engine.set_follow_authority(kind.map(|kind| match kind {
        Kind::Link | Kind::Clk => FollowProfile::FAST,
        Kind::Osc | Kind::Timecode => FollowProfile::SLOW,
    }));
}

/// This tick's sample, before the engine tick, as the content thread hands it over.
pub fn feed(engine: &mut PlaybackEngine, sample: Sample) {
    engine.set_external_sample(ExternalSample {
        beat: sample.beat,
        bpm: sample.bpm,
        fresh: sample.fresh,
        exact: sample.exact,
    });
}

/// An engine on a one-point map at `bpm`, with the stub renderers.
pub fn engine_with_map(bpm: f64, project: Option<Project>) -> PlaybackEngine {
    let mut project = project.unwrap_or_default();
    project.settings.bpm = Bpm(bpm as f32);
    project.tempo_map.add_or_replace_point(Beats::ZERO, Bpm(bpm as f32), TempoPointSource::Unknown, 0.001);
    let mut engine =
        PlaybackEngine::new(vec![Box::new(StubRenderer::new_generator()), Box::new(StubRenderer::new_video())]);
    engine.initialize(project);
    engine
}

/// What one tick showed.
#[derive(Debug, Clone, Copy)]
pub struct Shown {
    /// Wall seconds of the shown frame's deadline.
    pub deadline: f64,
    pub frames: u32,
    pub time: f64,
    pub beat: f64,
    /// The index moved by something other than `frames`, or the epoch bumped.
    pub jumped: bool,
    pub epoch: u64,
}

/// Drives the engine on the content tick grid with seeded wake jitter.
pub struct Driver {
    pub engine: PlaybackEngine,
    grid: FrameGrid,
    anchor: Instant,
    rng: Rng,
    tick: u64,
    frame_count: u64,
    /// Extra screen delay the reader adds to the deadline (D19); 0 on the stopgap.
    pub read_delay: f64,
    /// The true delay from deadline to screen, where the music is measured.
    pub screen_delay: f64,
    /// Presented-time jitter and the hidden extra delay the calibration sees (D19).
    pub present_jitter: f64,
    pub extra_delay: f64,
}

impl Driver {
    pub fn new(engine: PlaybackEngine, seed: u64) -> Self {
        let anchor = Instant::now();
        Self { engine, grid: FrameGrid::new(FPS, anchor), anchor, rng: Rng(seed | 1), tick: 0, frame_count: 0, read_delay: 0.0, screen_delay: 0.0, present_jitter: 0.0, extra_delay: 0.0 }
    }

    pub fn wall(&self) -> f64 {
        self.tick as f64 * STEP
    }

    /// One tick: wake with jitter (stretched by `stall` extra frames), read
    /// the source at the shown frame's read instant, feed it, tick.
    pub fn step(&mut self, reader: Option<&mut dyn Reader>, stall: u32) -> Shown {
        self.tick += 1 + u64::from(stall);
        let wake = self.tick as f64 * STEP + self.rng.range(0.0, 0.4) * STEP;
        let now = self.anchor + Duration::from_secs_f64(wake);
        let frames = self.grid.consume_at(now);
        let deadline = self.grid.consumed_deadline().saturating_duration_since(self.anchor).as_secs_f64();
        if let Some(reader) = reader
            && let Some(sample) = reader.read(deadline + self.read_delay, wake)
        {
            feed(&mut self.engine, sample);
        }
        let playing = self.engine.is_playing();
        let before = self.engine.transport_frame();
        let epoch = self.engine.transport_epoch();
        let result = self.engine.tick(TickContext {
            frames,
            realtime_now: Seconds(wake),
            frame_count: self.frame_count,
        });
        self.engine.reclaim_tick_result(result);
        self.frame_count += 1;
        let index = self.engine.transport_frame();
        Shown {
            deadline,
            frames,
            time: self.engine.current_time().0,
            beat: self.engine.frame_beat().0,
            jumped: playing && (index - before != i64::from(frames) || self.engine.transport_epoch() != epoch),
            epoch: self.engine.transport_epoch(),
        }
    }
}

/// Phase, speed and seek measurements against a true timeline.
#[derive(Debug, Default)]
pub struct Meter {
    pub max_phase_ms: f64,
    pub max_speed: f64,
    pub max_speed_change: f64,
    pub backward: u32,
    pub seeks: Vec<f64>,
    prev: Option<(Shown, f64, f64)>,
}

impl Meter {
    /// `truth` is the music's beat at the frame's screen instant; `warm`
    /// gates the phase and speed bounds (seeks and backward frames always count).
    pub fn observe(&mut self, shown: Shown, truth: f64, bpm: f64, warm: bool) {
        if shown.jumped {
            self.seeks.push(shown.deadline);
            self.prev = None;
            return;
        }
        if let Some((prev, prev_truth, prev_speed)) = self.prev {
            if shown.beat < prev.beat {
                self.backward += 1;
            }
            let music = truth - prev_truth;
            let speed = if music > 0.0 { (shown.beat - prev.beat) / music - 1.0 } else { 0.0 };
            if warm {
                self.max_phase_ms = self.max_phase_ms.max(((truth - shown.beat) * 60.0 / bpm * 1000.0).abs());
                if shown.frames == 1 && prev_speed.is_finite() {
                    self.max_speed = self.max_speed.max(speed.abs());
                    self.max_speed_change = self.max_speed_change.max((speed - prev_speed).abs());
                }
            }
            self.prev = Some((shown, truth, if shown.frames == 1 { speed } else { f64::NAN }));
        } else {
            self.prev = Some((shown, truth, f64::NAN));
        }
    }

    /// The music itself jumped (a relocation in Live): the next frame's
    /// increment is not a speed, so it starts a fresh measurement.
    pub fn music_jumped(&mut self) {
        self.prev = None;
    }

    pub fn report(&self) -> String {
        format!(
            "phase {:.2} ms, speed {:.3}%, per frame {:.3}%, backward {}, seeks {}",
            self.max_phase_ms,
            self.max_speed * 100.0,
            self.max_speed_change * 100.0,
            self.backward,
            self.seeks.len()
        )
    }
}

/// Play `secs` of wall time from the source with Live's timeline as the
/// truth, measured after `warm_up` seconds. Play is pressed on the first tick.
pub fn follow(engine: PlaybackEngine, reader: &mut dyn Reader, live: &Live, secs: f64, warm_up: f64, seed: u64) -> (Meter, Driver) {
    let mut driver = Driver::new(engine, seed);
    set_authority(&mut driver.engine, Some(reader.kind()));
    driver.engine.play();
    let mut meter = Meter::default();
    while driver.wall() < secs {
        let shown = driver.step(Some(reader), 0);
        let r = shown.deadline + driver.screen_delay;
        meter.observe(shown, live.beat(r), live.bpm(r), shown.deadline >= warm_up);
    }
    (meter, driver)
}
