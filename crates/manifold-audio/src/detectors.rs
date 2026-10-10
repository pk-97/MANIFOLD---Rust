//! Trained event detectors (kick now; snare, clap, hats later) behind one interface, and the
//! worker thread that runs one analysed send's detectors off the content thread
//! (docs/KICK_REALTIME_DESIGN.md section 1b). Offline paths run the same detectors inline.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::kick::KickDetector;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Kick,
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

impl EventDetector for KickDetector {
    fn push(&mut self, samples: &[f32], events: &mut Vec<DetectorEvent>) {
        self.push_with(samples, |f| events.push(DetectorEvent { kind: EventKind::Kick, sample: f.sample }));
    }

    fn max_events(&self, samples: usize) -> usize {
        self.max_fires(samples)
    }
}

static KICK_REFUSED: AtomicBool = AtomicBool::new(false);

/// The kick detector for mono input at `rate`, or `None` (logged once per process) when the
/// embedded model is refused. Without it the kick feature stays 0.
pub fn kick_detector(rate: u32) -> Option<KickDetector> {
    match KickDetector::new(rate) {
        Ok(d) => Some(d),
        Err(e) => {
            if !KICK_REFUSED.swap(true, Ordering::Relaxed) {
                log::error!("[Audio] kick detector disabled: {e}");
            }
            None
        }
    }
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
        let thread = std::thread::Builder::new().name("detector-worker".into()).spawn(move || {
            let mut buf = vec![0.0f32; WORK_STEP];
            let mut scratch = Vec::with_capacity(detectors.iter().map(|d| d.max_events(WORK_STEP)).sum());
            while worker.running.load(Ordering::Acquire) {
                let n = sample_cons.pop_slice(&mut buf);
                if n == 0 {
                    std::thread::park_timeout(Duration::from_millis(5));
                    continue;
                }
                scratch.clear();
                for d in detectors.iter_mut() {
                    d.push(&buf[..n], &mut scratch);
                }
                for e in scratch.drain(..) {
                    if event_prod.try_push(e).is_err() {
                        worker.event_overflow.fetch_add(1, Ordering::Relaxed);
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
            let last = if self.gap_len > 0 { self.gaps[self.gap_len - 1] } else { (u64::MAX, self.gap_base) };
            if last.0 == self.pushed || self.gap_len == GAPS {
                // Same position, or out of slots: fold into the last span. When out of slots,
                // events between the two drops are stamped late by this drop's length.
                self.gaps[self.gap_len.max(1) - 1] = (if self.gap_len > 0 { last.0 } else { self.pushed }, last.1 + n);
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
        let retired = self.gaps[..self.gap_len].iter().take_while(|g| g.0 <= p).count();
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
        self.gaps[..self.gap_len].iter().rev().find(|g| g.0 < s).map_or(self.gap_base, |g| g.1)
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
                    events.push(DetectorEvent { kind: EventKind::Kick, sample: self.n });
                }
            }
        }

        fn max_events(&self, samples: usize) -> usize {
            samples / self.every as usize + 1
        }
    }

    #[test]
    fn worker_returns_the_inline_events() {
        let mut inline = Ticker { every: 300, n: 0, delay: Duration::ZERO };
        let mut worker = DetectorWorker::spawn(vec![Box::new(Ticker { every: 300, n: 0, delay: Duration::ZERO })]).unwrap();
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
        let mut worker =
            DetectorWorker::spawn(vec![Box::new(Ticker { every: 100, n: 0, delay: Duration::from_millis(30) })]).unwrap();
        let mut got = Vec::with_capacity(64);
        worker.submit(&[0.0; 200]);
        let t0 = Instant::now();
        assert!(!worker.collect(t0 + Duration::from_millis(2), &mut got));
        assert!(t0.elapsed() < Duration::from_millis(20), "collect overran its deadline");
        assert!(got.is_empty());
        assert!(worker.collect(Instant::now() + Duration::from_secs(5), &mut got));
        assert_eq!(got.iter().map(|e| e.sample).collect::<Vec<_>>(), [100, 200]);
    }

    #[test]
    fn dropped_input_keeps_stamps_on_the_input_clock() {
        let mut worker =
            DetectorWorker::spawn(vec![Box::new(Ticker { every: 1000, n: 0, delay: Duration::from_millis(50) })]).unwrap();
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
        let want: Vec<u64> =
            (1..=(ring + 3000) / 1000).map(|k| k * 1000).map(|p| if p <= ring { p } else { p + 5000 }).collect();
        assert_eq!(got.iter().map(|e| e.sample).collect::<Vec<_>>(), want);
    }
}
