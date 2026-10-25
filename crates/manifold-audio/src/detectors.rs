//! Trained event detectors (kick and snare now; clap, hats later) behind one interface, and the
//! worker thread that runs one analysed send's detectors off the content thread. Offline paths run the same detectors inline.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::trained::container::{Container, ContainerError};
use crate::trained::{Decision, DetectorError, Fire, SpecError, TrainedDetector};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Kick,
    Snare,
}

impl EventKind {
    pub const ALL: [EventKind; 2] = [Self::Kick, Self::Snare];

    /// A model file's `family` text (D12).
    pub fn parse(family: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == family)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Kick => "kick",
            Self::Snare => "snare",
        }
    }
}

/// One detected event, stamped with the input sample count at which it was decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectorEvent {
    pub kind: EventKind,
    pub sample: u64,
}

/// Mono samples in at one fixed rate (sample 0 is the first sample pushed); events out.
pub trait EventDetector: Send {
    /// Appends the events decided within `samples`. Allocation-free when `events` has
    /// [`Self::max_events`] spare capacity.
    fn push(&mut self, samples: &[f32], events: &mut Vec<DetectorEvent>);
    /// The most events one push of `samples` samples can append.
    fn max_events(&self, samples: usize) -> usize;
}

/// The shipped models, one per family, built into the app. Each
/// file's `family` must equal its slot; `tests/fixtures/detectors/<family>/model.mkick` is the same bytes.
pub(crate) const EMBEDDED: [(EventKind, &[u8]); 2] = [
    (
        EventKind::Kick,
        include_bytes!("../assets/kick_model.mkick"),
    ),
    (
        EventKind::Snare,
        include_bytes!("../assets/snare_model.mkick"),
    ),
];

/// Each embedded file parsed once per process.
static MODELS: [OnceLock<Result<Container, ContainerError>>; 2] =
    [OnceLock::new(), OnceLock::new()];
/// Per family: a refusal or a load was logged.
static LOGGED: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

/// `kind`'s index in [`EMBEDDED`]. Exhaustive, so a new kind does not compile until it has a slot.
const fn slot(kind: EventKind) -> usize {
    match kind {
        EventKind::Kick => 0,
        EventKind::Snare => 1,
    }
}

/// The detector for `kind` on mono input at `rate`, from its embedded model. A refused model gives `Err`; the
/// family is then not built and its feature stays 0 (D8).
pub fn build_detector(kind: EventKind, rate: u32) -> Result<TrainedDetector, DetectorError> {
    let i = slot(kind);
    let model = MODELS[i]
        .get_or_init(|| Container::parse(EMBEDDED[i].1))
        .as_ref()
        .map_err(|e| DetectorError::from(e.clone()))?;
    let det = TrainedDetector::from_container(model, rate)?;
    if det.kind() != kind {
        return Err(DetectorError::Spec(SpecError::Mismatch {
            what: "family",
            detail: format!(
                "the {} slot holds a {} model",
                kind.name(),
                det.kind().name()
            ),
        }));
    }
    Ok(det)
}

/// [`build_detector`], or `None` with the refusal logged once per process. The first load logs the model and
/// its decision lag once.
pub fn detector(kind: EventKind, rate: u32) -> Option<TrainedDetector> {
    let built = build_detector(kind, rate);
    if !LOGGED[slot(kind)].swap(true, Ordering::Relaxed) {
        match &built {
            Ok(d) => log::info!(
                "[Audio] {} detector {}: decides {:.0} ms after emission",
                kind.name(),
                d.train_id(),
                d.decision_lag_s() * 1e3
            ),
            Err(e) => log::error!("[Audio] {} detector disabled: {e}", kind.name()),
        }
    }
    built.ok()
}

/// A model that reads the kick's verdicts runs right after the kick on the same samples: the kick's decisions
/// (fired or not) are handed over in emission order, then the model runs. One thread, no shared state.
pub struct KickFed {
    kick: TrainedDetector,
    reader: TrainedDetector,
}

/// Which detector of a [`KickFed`] made a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Kick,
    Reader,
}

impl KickFed {
    /// Input samples the kick runs ahead of the reader at most, whatever the push size: a handful of kick
    /// decisions, far inside the verdicts the reader keeps.
    const STEP: usize = 512;

    /// `reader` must want exactly `kick`'s model.
    pub fn new(kick: TrainedDetector, mut reader: TrainedDetector) -> Result<Self, DetectorError> {
        reader.link_kick(&kick)?;
        Ok(Self { kick, reader })
    }

    /// Runs the pair on `samples` step by step: the kick, its decisions handed to the reader in emission
    /// order, then the reader on the same step. Every decision of both goes to `on_decision`.
    pub fn push_traced(
        &mut self,
        samples: &[f32],
        mut on_decision: impl FnMut(Side, &Decision<'_>, Option<Fire>),
    ) {
        for step in samples.chunks(Self::STEP) {
            let reader = &mut self.reader;
            self.kick.push_traced(step, |d, fire| {
                reader.kick_verdict(d.cand_hop, d.avail_hop, d.p_final);
                on_decision(Side::Kick, d, fire);
            });
            self.reader.kick_settled(self.kick.settled_avail());
            self.reader
                .push_traced(step, |d, fire| on_decision(Side::Reader, d, fire));
        }
    }

    pub fn reader(&self) -> &TrainedDetector {
        &self.reader
    }
}

impl EventDetector for KickFed {
    fn push(&mut self, samples: &[f32], events: &mut Vec<DetectorEvent>) {
        let (kick_kind, reader_kind) = (self.kick.kind(), self.reader.kind());
        self.push_traced(samples, |side, _, fire| {
            let kind = if side == Side::Kick {
                kick_kind
            } else {
                reader_kind
            };
            events.extend(fire.map(|f| DetectorEvent {
                kind,
                sample: f.sample,
            }));
        });
    }

    fn max_events(&self, samples: usize) -> usize {
        self.kick.max_fires(samples) + self.reader.max_fires(samples)
    }
}

/// Every trained detector one analysed send runs, for mono input at `rate`: one per embedded model that loads.
/// A model reading kick verdicts runs fed by the kick ([`KickFed`]); without a matching kick it is refused
/// (logged once) and the kick runs alone. The live worker and the offline renderer both build theirs here.
pub fn send_detectors(rate: u32) -> Vec<Box<dyn EventDetector>> {
    let mut kick = detector(EventKind::Kick, rate);
    let mut out: Vec<Box<dyn EventDetector>> = Vec::new();
    let mut readers = Vec::new();
    for (kind, _) in EMBEDDED.iter().filter(|(k, _)| *k != EventKind::Kick) {
        let Some(d) = detector(*kind, rate) else {
            continue;
        };
        if d.wants_kick().is_some() {
            readers.push(d);
        } else {
            out.push(Box::new(d));
        }
    }
    for reader in readers {
        let kind = reader.kind();
        let Some(k) = kick.take() else {
            log::error!(
                "[Audio] {} detector disabled: it reads the kick's verdicts and the kick did not load",
                kind.name()
            );
            continue;
        };
        match KickFed::new(k, reader) {
            Ok(pair) => out.push(Box::new(pair)),
            Err(e) => {
                log::error!("[Audio] {} detector disabled: {e}", kind.name());
                kick = detector(EventKind::Kick, rate);
            }
        }
    }
    if let Some(k) = kick {
        out.insert(0, Box::new(k));
    }
    out
}

/// Input samples the worker's ring holds (about 2.7 s at 48 kHz).
const SAMPLE_RING: usize = 1 << 17;
const EVENT_RING: usize = 256;
/// Samples the worker hands its detectors per step.
const WORK_STEP: usize = 1024;
/// Dropped-input spans remembered for stamp mapping (see [`DetectorWorker::submit`]).
const GAPS: usize = 16;

struct Shared {
    running: AtomicBool,
    /// Ring samples the detectors have consumed; events are pushed before this advances.
    processed: AtomicU64,
    event_overflow: AtomicU64,
}

/// One thread running one send's detectors, fed through a lock-free sample ring and
/// answering through a lock-free event ring. Stops and joins on drop.
pub struct DetectorWorker {
    samples: HeapProd<f32>,
    events: HeapCons<DetectorEvent>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    /// Samples accepted into the ring (ring coordinates: dropped input excluded).
    pushed: u64,
    /// Dropped spans as `(ring position, input samples dropped up to and including it)`.
    gaps: [(u64, u64); GAPS],
    gap_len: usize,
    /// Dropped input before every retired gap.
    gap_base: u64,
    overflows: u64,
    dropped_samples: u64,
    events_lost_seen: u64,
}

impl DetectorWorker {
    pub fn spawn(mut detectors: Vec<Box<dyn EventDetector>>) -> std::io::Result<Self> {
        let (samples, mut sample_cons) = HeapRb::<f32>::new(SAMPLE_RING).split();
        let (mut event_prod, events) = HeapRb::<DetectorEvent>::new(EVENT_RING).split();
        let shared = Arc::new(Shared {
            running: AtomicBool::new(true),
            processed: AtomicU64::new(0),
            event_overflow: AtomicU64::new(0),
        });
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("detector-worker".into())
            .spawn(move || {
                let mut buf = vec![0.0f32; WORK_STEP];
                let mut scratch =
                    Vec::with_capacity(detectors.iter().map(|d| d.max_events(WORK_STEP)).sum());
                while worker.running.load(Ordering::Acquire) {
                    let n = sample_cons.pop_slice(&mut buf);
                    if n == 0 {
                        std::thread::park_timeout(Duration::from_millis(5));
                        continue;
                    }
                    // Each detector's events go out as soon as it has run, so a slow later detector (the
                    // snare's net) never holds an earlier one's fires (the kick's) past the content
                    // thread's wait.
                    for d in detectors.iter_mut() {
                        d.push(&buf[..n], &mut scratch);
                        for e in scratch.drain(..) {
                            if event_prod.try_push(e).is_err() {
                                worker.event_overflow.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    worker.processed.fetch_add(n as u64, Ordering::Release);
                }
            })?;
        Ok(Self {
            samples,
            events,
            shared,
            thread: Some(thread),
            pushed: 0,
            gaps: [(0, 0); GAPS],
            gap_len: 0,
            gap_base: 0,
            overflows: 0,
            dropped_samples: 0,
            events_lost_seen: 0,
        })
    }

    /// Hands the next input samples to the worker. Never blocks or allocates. When the ring
    /// cannot take the whole slice it is dropped, counted and logged; the worker's detectors
    /// see the two sides spliced, and event stamps still count the dropped samples.
    pub fn submit(&mut self, samples: &[f32]) {
        let n = samples.len() as u64;
        if n == 0 {
            return;
        }
        if self.samples.vacant_len() < samples.len() {
            self.overflows += 1;
            self.dropped_samples += n;
            let last = if self.gap_len > 0 {
                self.gaps[self.gap_len - 1]
            } else {
                (u64::MAX, self.gap_base)
            };
            if last.0 == self.pushed || self.gap_len == GAPS {
                // Same position, or out of slots: fold into the last span. When out of slots,
                // events between the two drops are stamped late by this drop's length.
                self.gaps[self.gap_len.max(1) - 1] = (
                    if self.gap_len > 0 {
                        last.0
                    } else {
                        self.pushed
                    },
                    last.1 + n,
                );
                self.gap_len = self.gap_len.max(1);
            } else {
                self.gaps[self.gap_len] = (self.pushed, last.1 + n);
                self.gap_len += 1;
            }
            if self.overflows.is_power_of_two() {
                log::warn!(
                    "[Audio] detector worker behind: dropped {} samples over {} submits",
                    self.dropped_samples,
                    self.overflows
                );
            }
            return;
        }
        self.samples.push_slice(samples);
        self.pushed += n;
        if let Some(t) = &self.thread {
            t.thread().unpark();
        }
    }

    /// Appends the events decided so far, waiting until the worker has consumed everything
    /// submitted or `deadline` passes, whichever is first. Returns whether it caught up; events
    /// that miss the deadline arrive on a later call. Allocation-free when `out` has room.
    pub fn collect(&mut self, deadline: Instant, out: &mut Vec<DetectorEvent>) -> bool {
        let mut spins = 0u32;
        let caught_up = loop {
            let p = self.shared.processed.load(Ordering::Acquire);
            if p >= self.pushed {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            spins += 1;
            if spins < 64 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        };
        // Every event not drained here comes from samples after `p`, so gaps at or before it
        // can retire once this drain is stamped.
        let p = self.shared.processed.load(Ordering::Acquire);
        while let Some(mut e) = self.events.try_pop() {
            e.sample += self.dropped_before(e.sample);
            out.push(e);
        }
        let retired = self.gaps[..self.gap_len]
            .iter()
            .take_while(|g| g.0 <= p)
            .count();
        if retired > 0 {
            self.gap_base = self.gaps[retired - 1].1;
            self.gaps.copy_within(retired..self.gap_len, 0);
            self.gap_len -= retired;
        }
        let lost = self.shared.event_overflow.load(Ordering::Relaxed);
        if lost != self.events_lost_seen {
            self.events_lost_seen = lost;
            log::warn!("[Audio] detector events lost: {lost} (events not collected)");
        }
        caught_up
    }

    /// Input samples dropped before ring position `s` (a span at `s` itself came after it).
    fn dropped_before(&self, s: u64) -> u64 {
        self.gaps[..self.gap_len]
            .iter()
            .rev()
            .find(|g| g.0 < s)
            .map_or(self.gap_base, |g| g.1)
    }

    /// Submits that were dropped because the ring was full.
    pub fn overflows(&self) -> u64 {
        self.overflows
    }
}

impl Drop for DetectorWorker {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::Release);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Emits an event at every multiple of `every` samples; sleeps `delay` on its first push.
    struct Ticker {
        every: u64,
        n: u64,
        delay: Duration,
    }

    impl EventDetector for Ticker {
        fn push(&mut self, samples: &[f32], events: &mut Vec<DetectorEvent>) {
            std::thread::sleep(std::mem::take(&mut self.delay));
            for _ in samples {
                self.n += 1;
                if self.n.is_multiple_of(self.every) {
                    events.push(DetectorEvent {
                        kind: EventKind::Kick,
                        sample: self.n,
                    });
                }
            }
        }

        fn max_events(&self, samples: usize) -> usize {
            samples / self.every as usize + 1
        }
    }

    #[test]
    fn every_event_kind_has_an_embedded_model() {
        assert_eq!(EventKind::ALL.len(), EMBEDDED.len());
        for kind in EventKind::ALL {
            assert_eq!(EMBEDDED[slot(kind)].0, kind);
            let det =
                build_detector(kind, 48_000).unwrap_or_else(|e| panic!("{}: {e}", kind.name()));
            assert_eq!(det.kind(), kind);
            assert_eq!(EventKind::parse(kind.name()), Some(kind));
        }
    }

    /// The kick and the snare reading it fire the same whatever the push size: one whole clip, or small blocks.
    #[test]
    fn kick_fed_pair_ignores_push_size() {
        use crate::trained::golden;
        let audio = golden::audio_f32(&golden::load("detectors/snare", "clip_dense.mkick"));
        let fires = |block: usize| {
            let mut dets = send_detectors(48_000);
            let mut events = Vec::new();
            for chunk in audio.chunks(block) {
                for d in &mut dets {
                    d.push(chunk, &mut events);
                }
            }
            let mut out: Vec<(&str, u64)> =
                events.iter().map(|e| (e.kind.name(), e.sample)).collect();
            out.sort_unstable();
            out
        };
        let whole = fires(audio.len());
        assert!(whole.iter().any(|e| e.0 == "snare"));
        assert_eq!(whole, fires(300));
    }

    #[test]
    fn worker_returns_the_inline_events() {
        let mut inline = Ticker {
            every: 300,
            n: 0,
            delay: Duration::ZERO,
        };
        let mut worker = DetectorWorker::spawn(vec![Box::new(Ticker {
            every: 300,
            n: 0,
            delay: Duration::ZERO,
        })])
        .unwrap();
        let input = vec![0.0f32; 10_000];
        let (mut want, mut got) = (Vec::new(), Vec::with_capacity(64));
        for chunk in input.chunks(733) {
            inline.push(chunk, &mut want);
            worker.submit(chunk);
            assert!(worker.collect(Instant::now() + Duration::from_secs(5), &mut got));
        }
        assert_eq!(got, want);
    }

    #[test]
    fn a_slow_worker_misses_the_deadline_and_delivers_later() {
        let mut worker = DetectorWorker::spawn(vec![Box::new(Ticker {
            every: 100,
            n: 0,
            delay: Duration::from_millis(30),
        })])
        .unwrap();
        let mut got = Vec::with_capacity(64);
        worker.submit(&[0.0; 200]);
        let t0 = Instant::now();
        assert!(!worker.collect(t0 + Duration::from_millis(2), &mut got));
        assert!(
            t0.elapsed() < Duration::from_millis(20),
            "collect overran its deadline"
        );
        assert!(got.is_empty());
        assert!(worker.collect(Instant::now() + Duration::from_secs(5), &mut got));
        assert_eq!(got.iter().map(|e| e.sample).collect::<Vec<_>>(), [100, 200]);
    }

    #[test]
    fn dropped_input_keeps_stamps_on_the_input_clock() {
        let mut worker = DetectorWorker::spawn(vec![Box::new(Ticker {
            every: 1000,
            n: 0,
            delay: Duration::from_millis(50),
        })])
        .unwrap();
        let mut got = Vec::with_capacity(512);
        // Fill the ring while the worker sleeps, so the next submit is dropped.
        let block = vec![0.0f32; SAMPLE_RING / 2];
        worker.submit(&block);
        worker.submit(&block);
        worker.submit(&[0.0; 5000]);
        assert_eq!(worker.overflows(), 1);
        while !worker.collect(Instant::now() + Duration::from_secs(10), &mut got) {}
        worker.submit(&[0.0; 3000]);
        assert!(worker.collect(Instant::now() + Duration::from_secs(10), &mut got));
        let ring = SAMPLE_RING as u64;
        // Ring position p maps to input p before the drop and p + 5000 after it.
        let want: Vec<u64> = (1..=(ring + 3000) / 1000)
            .map(|k| k * 1000)
            .map(|p| if p <= ring { p } else { p + 5000 })
            .collect();
        assert_eq!(got.iter().map(|e| e.sample).collect::<Vec<_>>(), want);
    }
}
