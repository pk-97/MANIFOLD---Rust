//! Scene hits on a fixed-tick simulation clock, whatever the simulation: a
//! GPU liquid, a swarm. A hit fired after a frame is stamped at that frame's
//! simulated time, so it lands on the first tick the next frame runs, once.
//! While the clock is held (pause, Speed 0) a hit is discarded with a
//! receipt, so resume never replays it.
//!
//! A hit reads "applied" only through [`TickHits::commit_frame`], which the
//! simulation calls once its frame's ticks are encoded. A frame that fails
//! first never commits: its hits are discarded with a receipt at the next
//! drain or frame.

use crate::clock::ClockFrame;
use crate::clock::TICK;
use crate::input::{AppliedEvent, EventQueue, EventStamp};
use crate::{Seconds, TickStamp};

/// Hits in flight at once: queued, this frame's and undrained applied
/// receipts. Discard receipts hold no slot. A hit past it is refused on its
/// own; the next one is tried again.
pub const HIT_CAPACITY: usize = 256;

/// What [`TickHits::admit`] decided for one hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// The hit may be queued; it lands on this tick or later.
    Open(TickStamp),
    /// The clock is held: the hit was discarded with a receipt and the
    /// producer still succeeds, so it rearms.
    Discarded(TickStamp),
}

/// The clock-side queue of one simulation's hits. `label` names it in every
/// refusal ("Liquid impulses", "Swarm impulses").
pub struct TickHits<P> {
    label: &'static str,
    queue: Option<EventQueue<P>>,
    /// Counts restarts; never wraps in practice, unlike the clock's u32.
    epoch: u64,
    held: bool,
    /// Transport of the frame last observed: no older hit is stamped.
    transport: Option<f64>,
    simulation_time: f64,
    last_sequence: Option<u64>,
    /// This frame's hits, taken by its begun ticks, until the frame commits.
    pending: Vec<AppliedEvent<P>>,
    /// Receipts of committed frames, undrained.
    applied: Vec<AppliedEvent<P>>,
    impulse_tick: Option<u64>,
    /// Discard receipts, undrained, at most `HIT_CAPACITY`; sized when the
    /// clock starts so a held fire never allocates.
    discarded: Vec<EventStamp>,
    /// Discards past that bound, counted without a stamp.
    discarded_beyond: u64,
    /// The clock broke (a tick out of order): every hit is refused until a
    /// restart. A full history is never this; it refuses one hit at a time.
    failure: Option<String>,
}

/// Keep one discard receipt within the bound, else count it without a stamp.
fn record_discard(discarded: &mut Vec<EventStamp>, beyond: &mut u64, stamp: EventStamp) {
    if discarded.len() < HIT_CAPACITY {
        discarded.push(stamp);
    } else {
        *beyond += 1;
    }
}

impl<P> TickHits<P> {
    pub const fn new(label: &'static str) -> Self {
        Self {
            label,
            queue: None,
            epoch: 0,
            held: false,
            transport: None,
            simulation_time: 0.0,
            last_sequence: None,
            pending: Vec::new(),
            applied: Vec::new(),
            impulse_tick: None,
            discarded: Vec::new(),
            discarded_beyond: 0,
            failure: None,
        }
    }

    /// Begin this frame's ticks, right after the clock advanced to `frame`. A
    /// restart opens a new epoch and cancels every unread event of the old one.
    pub fn observe_frame(
        &mut self,
        played: crate::PlayedTime,
        frame: &ClockFrame,
    ) -> Result<(), String> {
        let label = self.label;
        if frame.restarted {
            self.epoch += 1;
            match &mut self.queue {
                Some(queue) => {
                    queue
                        .reset(self.epoch, Seconds::ZERO)
                        .map_err(|error| format!("{label}: {error}"))?;
                }
                None => {
                    self.queue = Some(
                        EventQueue::new(self.epoch, Seconds::ZERO, Seconds(TICK), HIT_CAPACITY)
                            .map_err(|error| format!("{label}: {error}"))?,
                    );
                    self.discarded.reserve_exact(HIT_CAPACITY);
                }
            }
            self.pending.clear();
            self.applied.clear();
            self.discarded.clear();
            self.discarded_beyond = 0;
            self.last_sequence = None;
            self.failure = None;
        }
        self.abandon_frame();
        self.held = frame.held;
        self.transport = Some(played.seconds());
        self.simulation_time = frame.simulation_time;
        self.impulse_tick = None;
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let queue = self
            .queue
            .as_mut()
            .ok_or_else(|| format!("{label}: the clock never started"))?;
        let first = frame.first_sequence;
        if queue.next_tick().tick != first {
            let error = format!(
                "{label}: tick {first} does not follow tick {}; restart the simulation",
                queue.next_tick().tick
            );
            self.failure = Some(error.clone());
            return Err(error);
        }
        let pending = &mut self.pending;
        for tick in first..first + u64::from(frame.ticks) {
            let stamp = TickStamp {
                epoch: self.epoch,
                tick,
            };
            let result = if let Some(interval) = frame.interval(tick - first) {
                queue.begin_interval(stamp, interval, |event| pending.push(event))
            } else {
                Ok(())
            };
            if let Err(error) = result {
                let error = format!("{label}: {error}");
                self.failure = Some(error.clone());
                return Err(error);
            }
        }
        self.impulse_tick = self.pending.first().map(|event| event.applied.tick);
        Ok(())
    }

    /// This frame's ticks are encoded: its hits are applied.
    pub fn commit_frame(&mut self) {
        self.applied.append(&mut self.pending);
    }

    /// This frame's ticks will not run: its hits are discarded, with a receipt.
    pub fn abandon_frame(&mut self) {
        let Self {
            pending,
            discarded,
            discarded_beyond,
            ..
        } = self;
        for event in pending.drain(..) {
            record_discard(discarded, discarded_beyond, event.source);
        }
    }

    /// The epoch stamps must carry, once the clock has started.
    pub fn epoch(&self) -> Option<u64> {
        self.queue.as_ref().map(|_| self.epoch)
    }

    /// Stamp a hit at the simulated time of the frame last observed. Every hit
    /// has no tick yet whatever its transport, so it lands on the next
    /// frame's first tick. A hit older than that frame is late, never
    /// skipped: triggers are the performance.
    pub fn stamp(&self, played: crate::PlayedTime, sequence: u64) -> Result<EventStamp, String> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self.queue.is_none() || self.transport.is_none() {
            return Err(format!("{}: the clock has not started", self.label));
        }
        if !played.is_finite() {
            return Err(format!("{}: the hit time is not finite", self.label));
        }
        Ok(EventStamp {
            epoch: self.epoch,
            time: Seconds(self.simulation_time),
            sequence,
        })
    }

    /// Whether hits are refused because the clock broke; cleared by a restart.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// Check one hit before anything takes it. A held clock discards it here.
    /// `also_outstanding` counts hits a caller keeps in flight elsewhere
    /// against the same capacity.
    pub fn admit(
        &mut self,
        stamp: EventStamp,
        also_outstanding: usize,
    ) -> Result<Admission, String> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let label = self.label;
        let Some(queue) = self.queue.as_ref() else {
            return Err(format!("{label}: the clock has not started"));
        };
        if stamp.epoch != self.epoch {
            return Err(format!(
                "{label}: the hit belongs to a restarted simulation"
            ));
        }
        if self
            .last_sequence
            .is_some_and(|last| stamp.sequence <= last)
        {
            return Err(format!("{label}: hit sequences must increase"));
        }
        let next = queue.next_tick();
        if self.held {
            self.last_sequence = Some(stamp.sequence);
            record_discard(&mut self.discarded, &mut self.discarded_beyond, stamp);
            return Ok(Admission::Discarded(next));
        }
        let outstanding = queue.len() + self.pending.len() + self.applied.len() + also_outstanding;
        if outstanding >= HIT_CAPACITY {
            return Err(format!(
                "{label}: {HIT_CAPACITY} hits are already in flight; this hit is refused"
            ));
        }
        Ok(Admission::Open(next))
    }

    /// Take an admitted hit. `None` takes its sequence only (a hit another
    /// owner carries).
    pub fn take(&mut self, stamp: EventStamp, hit: Option<P>) -> Result<Option<TickStamp>, String> {
        self.last_sequence = Some(stamp.sequence);
        let Some(hit) = hit else { return Ok(None) };
        let label = self.label;
        self.queue
            .as_mut()
            .ok_or_else(|| format!("{label}: the clock has not started"))?
            .enqueue(stamp, hit)
            .map(Some)
            .map_err(|error| format!("{label}: {error}"))
    }

    /// Admit and take one hit.
    pub fn enqueue(&mut self, stamp: EventStamp, hit: P) -> Result<TickStamp, String> {
        match self.admit(stamp, 0)? {
            Admission::Discarded(next) => Ok(next),
            Admission::Open(_) => Ok(self.take(stamp, Some(hit))?.expect("a taken hit is queued")),
        }
    }

    /// Receipts of committed frames. A frame drained uncommitted failed.
    pub fn drain_applied(&mut self, consume: &mut dyn FnMut(AppliedEvent<P>)) {
        self.abandon_frame();
        for event in self.applied.drain(..) {
            consume(event);
        }
    }

    /// Discard receipts not yet drained, after abandoning an uncommitted frame.
    pub fn discarded(&mut self) -> &[EventStamp] {
        self.abandon_frame();
        &self.discarded
    }

    /// Stamps of hits discarded while the clock was held or by a failed
    /// frame. Returns the count of further discards past the bounded receipt
    /// list: discarded hits with no stamp.
    pub fn drain_discarded(&mut self, consume: &mut dyn FnMut(EventStamp)) -> u64 {
        self.abandon_frame();
        self.discarded.drain(..).for_each(&mut *consume);
        std::mem::take(&mut self.discarded_beyond)
    }

    /// The tick this frame's hits land on, if any.
    pub fn impulse_tick(&self) -> Option<u64> {
        self.impulse_tick
    }

    /// The raw queue, once the clock has started: places a hit at its own
    /// stamp's time, past the stamp-at-the-frame rule (replays and tests).
    pub fn queue_mut(&mut self) -> Option<&mut EventQueue<P>> {
        self.queue.as_mut()
    }

    /// Receipt storage: sized when the clock starts, so a held hit never
    /// allocates.
    pub fn discarded_capacity(&self) -> usize {
        self.discarded.capacity()
    }

    /// This frame's hits, until the frame commits or is abandoned.
    pub fn frame_events(&self) -> &[AppliedEvent<P>] {
        &self.pending
    }
}
