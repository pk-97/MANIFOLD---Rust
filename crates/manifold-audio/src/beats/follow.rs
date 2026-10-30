//! Live beat following. A `Listener` takes the audio stream and, when asked, runs Beat This! over its newest window:
//! the expensive part, which other live detectors can read too. A `Tracker` turns each hearing into a lock: the
//! steady grid of the newest beats, with the held tempo kept against a few hearings of another one and bar 1 kept
//! against a few votes for another beat of the bar. The network looks
//! both ways, so the beats it finds near the present are its weakest; the lock is a grid, and the beat now and the
//! next ones come from extending it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::mel::{BANDS, FPS, MelFramer, N_FFT, RATE};
use super::model::Model;
use super::resample::Resampler;
use super::{BORDER, Grid, NoGrid, fit};

/// How the follower trades reaction time against steadiness and CPU.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tuning {
    /// Seconds of recent audio the network reads each hearing.
    pub window_s: f64,
    /// Seconds of the newest beats the grid is fitted to.
    pub fit_s: f64,
    /// Beats the fit span must hold for a lock.
    pub min_beats: usize,
    /// Hearings in a row that must agree before bar 1 moves to another beat of the bar.
    pub bar_votes: u32,
    /// Hearings in a row that must agree on a tempo more than `SAME_TEMPO` from the held one before the follower
    /// takes it.
    pub tempo_votes: u32,
    /// Seconds of stream between hearings.
    pub hear_every_s: f64,
}

impl Tuning {
    /// The best steady tuning on four recordings (three songs and a 21 minute live set) in `examples/follow_sim`:
    /// about three quarters of predicted beats within 70 ms, five tempo jumps in 33 minutes. A shorter window
    /// costs less CPU but hears more stray tempos.
    pub const LIVE: Self = Self {
        window_s: 12.0,
        fit_s: 8.0,
        min_beats: 8,
        bar_votes: 5,
        tempo_votes: 6,
        hear_every_s: 1.0,
    };
}

/// The grid the follower hears, in stream seconds: seconds since the first sample pushed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lock {
    pub grid: Grid,
    /// Share of the fitted beats on the grid.
    pub on_grid: f64,
    /// The beat number of `grid.downbeat_s`: one count runs on across locks, with bar 1 on whole bars of it.
    pub downbeat_beat: f64,
}

impl Lock {
    /// The beat number at stream second `t`.
    pub fn beat_at(&self, t: f64) -> f64 {
        self.downbeat_beat + (t - self.grid.downbeat_s) / self.grid.period_s
    }
}

/// The network's beat and downbeat logits over one window, a frame every 20 ms; the beat logit includes the
/// downbeat one.
#[derive(Debug, Clone, Default)]
pub struct Heard {
    pub beat: Vec<f32>,
    pub downbeat: Vec<f32>,
    /// Stream seconds of the first frame's centre.
    pub start_s: f64,
}

/// The audio stream as log-mel frames, and the network over the newest window of them.
pub struct Listener {
    model: Model,
    resampler: Resampler,
    framer: MelFramer,
    resampled: Vec<f32>,
    /// The newest log-mel frames, oldest first, at most `window` of them.
    mel: Vec<f32>,
    window: usize,
    /// Frames made since the start; the window's first frame is `made - window length`.
    made: u64,
}

impl Listener {
    /// A listener for mono audio at `rate` Hz that hears the newest `window_s` seconds.
    pub fn new(rate: u32, window_s: f64) -> Self {
        let window = (window_s * FPS) as usize;
        Self {
            model: Model::load().expect("the shipped Beat This! weights parse"),
            resampler: Resampler::new(rate, RATE),
            framer: MelFramer::new(),
            resampled: Vec::new(),
            mel: Vec::with_capacity((window + 64) * BANDS),
            window,
            made: 0,
        }
    }

    /// Adds mono samples to the stream.
    pub fn push(&mut self, samples: &[f32]) {
        self.resampled.clear();
        self.resampler.push(samples, &mut self.resampled);
        let before = self.mel.len();
        self.framer.push(&self.resampled, &mut self.mel);
        self.made += ((self.mel.len() - before) / BANDS) as u64;
        let excess = self.mel.len().saturating_sub(self.window * BANDS);
        self.mel.drain(..excess);
    }

    /// The network over the newest window; about 60 ms of CPU per second of window, so call it from a worker
    /// thread.
    pub fn hear(&self) -> Heard {
        let frames = self.mel.len() / BANDS;
        let (beat, downbeat) = if frames > 2 * BORDER {
            self.model.logits(&self.mel)
        } else {
            (Vec::new(), Vec::new())
        };
        // Window frame 0 is stream frame `made - frames`, centred N_FFT/2 samples after its start.
        let start_s =
            (self.made - frames as u64) as f64 / FPS + (N_FFT / 2) as f64 / f64::from(RATE);
        Heard {
            beat,
            downbeat,
            start_s,
        }
    }
}

/// Turns hearings into locks, holding the tempo and bar 1 across them.
pub struct Tracker {
    tuning: Tuning,
    beats_per_bar: u32,
    tempo: TempoHold,
    bar: BarVote,
    last: Option<Lock>,
}

impl Tracker {
    pub fn new(tuning: Tuning, beats_per_bar: u32) -> Self {
        Self {
            tuning,
            beats_per_bar: beats_per_bar.max(1),
            tempo: TempoHold::default(),
            bar: BarVote::default(),
            last: None,
        }
    }

    pub fn lock(&mut self, heard: &Heard) -> Result<Lock, NoGrid> {
        let frames = heard.beat.len();
        let fit_frames = (self.tuning.fit_s * FPS) as usize;
        if frames < fit_frames.max(2 * BORDER + 1) {
            return Err(NoGrid::TooShort);
        }
        // The network was not trained on its outer frames, and the fit reads only the newest beats; the downbeat
        // vote reads the whole window.
        let mut beat = heard.beat.clone();
        let mut downbeat = heard.downbeat.clone();
        beat[..(frames - fit_frames).max(BORDER)].fill(f32::NEG_INFINITY);
        for edge in [&mut beat, &mut downbeat] {
            edge[frames - BORDER..].fill(f32::NEG_INFINITY);
            edge[..BORDER].fill(f32::NEG_INFINITY);
        }
        let (local, on_grid) = fit(&beat, &downbeat, self.beats_per_bar, self.tuning.min_beats)?;
        let heard_grid = Grid {
            downbeat_s: local.downbeat_s + heard.start_s,
            ..local
        };
        let grid = self.tempo.settle(heard_grid, self.tuning.tempo_votes);
        let downbeat_s = self.bar.settle(
            grid.downbeat_s,
            grid.period_s,
            self.beats_per_bar,
            self.tuning.bar_votes,
        );
        // This bar 1 takes the whole bar of the count nearest where the last lock put it.
        let bar = f64::from(self.beats_per_bar);
        let lock = Lock {
            grid: Grid { downbeat_s, ..grid },
            on_grid,
            downbeat_beat: self
                .last
                .map_or(0.0, |last| (last.beat_at(downbeat_s) / bar).round() * bar),
        };
        self.last = Some(lock);
        Ok(lock)
    }
}

/// Seconds of audio the worker may fall behind by; a hearing takes most of a second of CPU.
const SAMPLE_RING_S: u32 = 8;

/// A listener and a tracker with the live tuning on their own thread, fed through a lock-free sample ring and
/// answering through a lock-free lock ring. Stream seconds count the samples the ring took.
pub struct FollowWorker {
    samples: HeapProd<f32>,
    locks: HeapCons<Result<Lock, NoGrid>>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    rate: u32,
    accepted: u64,
    overflows: u64,
}

impl FollowWorker {
    /// A worker for mono audio at `rate` Hz, in bars of `beats_per_bar` beats.
    pub fn spawn(rate: u32, beats_per_bar: u32) -> std::io::Result<Self> {
        let tuning = Tuning::LIVE;
        let (samples, mut input) = HeapRb::<f32>::new((rate * SAMPLE_RING_S) as usize).split();
        let (mut output, locks) = HeapRb::<Result<Lock, NoGrid>>::new(4).split();
        let running = Arc::new(AtomicBool::new(true));
        let alive = running.clone();
        let thread = std::thread::Builder::new()
            .name("beat-follow".into())
            .spawn(move || {
                let mut listener = Listener::new(rate, tuning.window_s);
                let mut tracker = Tracker::new(tuning, beats_per_bar);
                let every = (tuning.hear_every_s * f64::from(rate)) as u64;
                let mut buf = vec![0.0f32; 4096];
                let (mut fed, mut next) = (0u64, every);
                while alive.load(Ordering::Acquire) {
                    let n = input.pop_slice(&mut buf);
                    if n == 0 {
                        std::thread::park_timeout(Duration::from_millis(20));
                        continue;
                    }
                    listener.push(&buf[..n]);
                    fed += n as u64;
                    if fed >= next {
                        next = fed + every;
                        // The reader takes the newest each frame, so the ring is full only when it has stopped
                        // reading; the hearing is dropped then.
                        let _ = output.try_push(tracker.lock(&listener.hear()));
                    }
                }
            })?;
        Ok(Self {
            samples,
            locks,
            running,
            thread: Some(thread),
            rate,
            accepted: 0,
            overflows: 0,
        })
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Hands the next mono samples to the worker. Never blocks or allocates. When the ring cannot take them they
    /// are dropped and logged; the worker hears the two sides spliced until its window has passed the gap.
    pub fn submit(&mut self, mono: &[f32]) {
        if self.samples.vacant_len() < mono.len() {
            self.overflows += 1;
            if self.overflows.is_power_of_two() {
                log::warn!(
                    "[Audio] beat follower behind: dropped audio {} times",
                    self.overflows
                );
            }
            return;
        }
        self.samples.push_slice(mono);
        self.accepted += mono.len() as u64;
        if let Some(t) = &self.thread {
            t.thread().unpark();
        }
    }

    /// Stream seconds at the end of the samples submitted so far.
    pub fn stream_s(&self) -> f64 {
        self.accepted as f64 / f64::from(self.rate)
    }

    /// The newest hearing's lock, if one finished since the last call.
    pub fn latest(&mut self) -> Option<Result<Lock, NoGrid>> {
        let mut newest = None;
        while let Some(lock) = self.locks.try_pop() {
            newest = Some(lock);
        }
        newest
    }
}

impl Drop for FollowWorker {
    /// Tells the thread to stop and leaves it to finish its hearing; joining would stall the caller for up to one.
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
        }
    }
}

/// A tempo this close to the held one, as a fraction, is the same tempo moving.
const SAME_TEMPO: f64 = 0.03;

/// Holds the tempo. A hearing within `SAME_TEMPO` of the held tempo is taken at once, so a pitch ride is followed.
/// Any other waits until enough hearings in a row agree on it. Meanwhile a hearing of double, half, four times or a
/// quarter of the held tempo (the network hearing every other beat, or the half or quarter beats, as it does in
/// thin passages) is folded back onto the held tempo, and any other keeps the held grid.
#[derive(Default)]
pub(super) struct TempoHold {
    held: Option<Grid>,
    /// The period the hearings away from the held tempo agree on, and how many in a row have.
    rival: Option<(f64, u32)>,
}

impl TempoHold {
    pub(super) fn settle(&mut self, heard: Grid, votes: u32) -> Grid {
        let same = |a: f64, b: f64| (a / b - 1.0).abs() < SAME_TEMPO;
        let Some(held) = self.held.filter(|h| !same(heard.period_s, h.period_s)) else {
            self.rival = None;
            self.held = Some(heard);
            return heard;
        };
        let count = match self.rival {
            Some((period_s, n)) if same(heard.period_s, period_s) => n + 1,
            _ => 1,
        };
        if count >= votes {
            self.rival = None;
            self.held = Some(heard);
            return heard;
        }
        self.rival = Some((heard.period_s, count));
        let ratio = heard.period_s / held.period_s;
        let Some(factor) = [2.0f64, 4.0, 0.5, 0.25]
            .into_iter()
            .find(|f| (ratio / f - 1.0).abs() < SAME_TEMPO)
        else {
            return held;
        };
        // Heard slower, every heard beat is a held beat. Heard faster, the heard downbeat may fall between held
        // beats; take whichever heard beat of the next held beat's worth sits on the held grid.
        let off = |t: f64| {
            let x = (t - held.downbeat_s) / held.period_s;
            (x - x.round()).abs()
        };
        let steps = (1.0 / factor).round().max(1.0) as usize;
        let downbeat_s = (0..steps)
            .map(|j| heard.downbeat_s + j as f64 * heard.period_s)
            .min_by(|a, b| off(*a).total_cmp(&off(*b)))
            .expect("at least one step");
        let folded = Grid {
            period_s: heard.period_s / factor,
            downbeat_s,
            ..heard
        };
        self.held = Some(folded);
        folded
    }
}

/// Holds bar 1 against a few votes for another beat of the bar.
#[derive(Default)]
pub(super) struct BarVote {
    held: Option<f64>,
    /// A rival bar 1 and how many hearings in a row have voted for it.
    rival: Option<(f64, u32)>,
}

impl BarVote {
    /// The downbeat to report given this hearing's `vote`, on a grid of `period_s`.
    pub(super) fn settle(
        &mut self,
        vote: f64,
        period_s: f64,
        beats_per_bar: u32,
        votes: u32,
    ) -> f64 {
        let bar = i64::from(beats_per_bar);
        // How many beats of the bar `t` sits after `vote`.
        let offset = |t: f64| (((t - vote) / period_s).round() as i64).rem_euclid(bar);
        let Some(held) = self.held else {
            self.held = Some(vote);
            return vote;
        };
        if offset(held) == 0 {
            self.held = Some(vote);
            self.rival = None;
            return vote;
        }
        let count = match self.rival {
            Some((rival, n)) if offset(rival) == 0 => n + 1,
            _ => 1,
        };
        if count >= votes {
            self.held = Some(vote);
            self.rival = None;
            return vote;
        }
        self.rival = Some((vote, count));
        // The held bar 1 carried onto this hearing's grid.
        let kept = vote + offset(held) as f64 * period_s;
        self.held = Some(kept);
        kept
    }
}
