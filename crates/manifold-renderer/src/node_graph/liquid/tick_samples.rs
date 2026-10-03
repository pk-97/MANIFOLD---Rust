//! Authored values at each tick's start, never per display frame: a GPU
//! liquid's forces and its colliders' poses read the same values at any
//! frame rate. Before each frame the host's physics history replay asks
//! [`TickSamples::request`] for the transport times the coming ticks start
//! at, runs the authored ancestry at exactly those times, and hands each
//! result to [`TickSamples::observe`]. Once the clock has advanced,
//! [`TickSamples::settle`] adds this frame's own value for a tick that starts
//! now. A tick nobody sampled is an error, never a guess.

use std::collections::VecDeque;

use crate::node_graph::liquid::clock::{ClockFrame, LiquidClock};

/// A tick start this close to the frame's transport (seconds) starts now:
/// the frame's own value belongs to it.
const NOW: f64 = 1e-9;

pub struct TickSamples<T> {
    /// The value at each tick's start, ascending by tick, from the oldest
    /// tick still needed.
    ticks: VecDeque<(u64, T)>,
    /// Transport times the replay must sample before the next frame, with
    /// their ticks, ascending.
    requests: Vec<(f64, u64)>,
}

impl<T> Default for TickSamples<T> {
    fn default() -> Self {
        Self { ticks: VecDeque::new(), requests: Vec::new() }
    }
}

impl<T: Clone> TickSamples<T> {
    /// Each not-yet-run tick's start under `clock` in `(from, until]`, into
    /// `out`. Replaces any earlier requests.
    pub fn request(&mut self, clock: &LiquidClock, from: f64, until: f64, out: &mut Vec<f64>) {
        self.requests.clear();
        let requests = &mut self.requests;
        clock.tick_starts(from, until, |transport, tick| {
            requests.push((transport, tick));
            out.push(transport);
        });
    }

    /// A replay sample at transport `now`: `value` belongs to every requested
    /// tick whose start has been reached. The replay samples each request
    /// exactly; the closing sample takes one rounding past the frame. `None`
    /// (an input still pending) records nothing, so those ticks stay unsampled.
    pub fn observe(&mut self, now: f64, value: Option<&T>) {
        let reached = self.requests.partition_point(|&(transport, _)| transport <= now);
        if reached == 0 {
            return;
        }
        let mut requests = std::mem::take(&mut self.requests);
        if let Some(value) = value {
            for &(_, tick) in &requests[..reached] {
                self.record(tick, value);
            }
        }
        requests.drain(..reached);
        self.requests = requests;
    }

    /// Call right after `clock` advanced to `frame`, with this frame's value.
    /// A restart forgets every sample. The value belongs to any tick starting
    /// now; every other start was or will be sampled by the replay.
    /// `None` (nothing wired) forgets every sample.
    pub fn settle(&mut self, clock: &LiquidClock, frame: &ClockFrame, value: Option<&T>) {
        let Some(value) = value.filter(|_| !frame.restarted) else {
            self.ticks.clear();
            if let (true, Some(value)) = (frame.restarted, value) {
                self.settle_now(clock, value);
            }
            return;
        };
        self.settle_now(clock, value);
    }

    fn settle_now(&mut self, clock: &LiquidClock, value: &T) {
        let now = clock.transport();
        let mut tick = clock.ticks_done();
        while let Some(start) = clock.tick_start(tick).filter(|&start| start <= now + NOW) {
            if (start - now).abs() <= NOW {
                self.record(tick, value);
            }
            tick += 1;
        }
    }

    /// The `(tick, value)` pairs for ticks `first..first + count`, or the
    /// first tick nobody sampled.
    pub fn span(&self, first: u64, count: usize) -> Result<std::collections::vec_deque::Iter<'_, (u64, T)>, u64> {
        let start = self.ticks.partition_point(|(recorded, _)| *recorded < first);
        for offset in 0..count {
            let tick = first + offset as u64;
            if self.ticks.get(start + offset).is_none_or(|(recorded, _)| *recorded != tick) {
                return Err(tick);
            }
        }
        Ok(self.ticks.range(start..start + count))
    }

    /// The value at `tick`'s start, if sampled.
    pub fn get(&self, tick: u64) -> Option<&T> {
        self.span(tick, 1).ok().and_then(|mut span| span.next()).map(|(_, value)| value)
    }

    /// Forget ticks before `tick`.
    pub fn prune_before(&mut self, tick: u64) {
        while self.ticks.front().is_some_and(|(recorded, _)| *recorded < tick) {
            self.ticks.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.ticks.clear();
    }

    /// A later observation of the same boundary replaces its held value.
    fn record(&mut self, tick: u64, value: &T) {
        let at = self.ticks.partition_point(|(recorded, _)| *recorded < tick);
        match self.ticks.get_mut(at) {
            Some((recorded, held)) if *recorded == tick => held.clone_from(value),
            _ => self.ticks.insert(at, (tick, value.clone())),
        }
    }
}
