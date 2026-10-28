//! The longest single submit in a span of encoding, read back on the content
//! thread, as on Metal: one long submit is what starves everything else that
//! shares the GPU, while many short ones that add up to a slow frame do not.
//! Every submit of an open span carries timestamps (the encoder's per-submit
//! stamp); the queue's waiter folds its GPU time into the span's slot, and
//! [`GpuSpanTimer::poll`] hands out finished slots in submission order. A
//! submit that failed, or ran on a lost device, counts as infinitely long.
//! Slots are allocated once; no frame allocates.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::*;

/// Spans awaiting readback. Past this a new span goes untimed.
const SLOTS: usize = 64;

/// One span's reading, written by the queue's waiter.
pub(crate) struct SpanSlot {
    /// Submits still on the GPU, plus one until the span's last submit.
    pending: AtomicU32,
    /// Bits of the longest submit's GPU seconds. Non-negative `f64`s order
    /// like their bits.
    longest: AtomicU64,
}

impl SpanSlot {
    /// A submit of the span goes to the GPU.
    pub(crate) fn submitted(&self) {
        self.pending.fetch_add(1, Ordering::Relaxed);
    }

    /// A submit of the span finished after `seconds` on the GPU.
    pub(crate) fn finished(&self, seconds: f64) {
        self.longest.fetch_max(seconds.to_bits(), Ordering::Relaxed);
        self.pending.fetch_sub(1, Ordering::Release);
    }

    /// The span's last submit went to the GPU.
    pub(crate) fn closed(&self) {
        self.pending.fetch_sub(1, Ordering::Release);
    }
}

#[derive(Default)]
pub struct GpuSpanTimer {
    slots: Vec<Arc<SpanSlot>>,
    free: Vec<usize>,
    /// The open span: key, frame and slot.
    open: Option<(u64, u64, usize)>,
    /// Ended spans in submission order.
    ended: VecDeque<(u64, u64, usize)>,
}

impl GpuSpanTimer {
    /// Start span `key` of `frame` in the encoder's current command buffer.
    pub fn begin(&mut self, encoder: &mut GpuEncoder, key: u64, frame: u64) {
        self.end(encoder);
        if self.slots.is_empty() {
            self.slots = (0..SLOTS)
                .map(|_| {
                    Arc::new(SpanSlot {
                        pending: AtomicU32::new(0),
                        longest: AtomicU64::new(0),
                    })
                })
                .collect();
            self.free = (0..SLOTS).rev().collect();
            self.ended.reserve(SLOTS);
        }
        let Some(index) = self.free.pop() else {
            return;
        };
        let slot = &self.slots[index];
        slot.pending.store(1, Ordering::Relaxed);
        slot.longest.store(0, Ordering::Relaxed);
        encoder.span = Some(slot.clone());
        self.open = Some((key, frame, index));
    }

    /// End the open span in the encoder's current command buffer, which the
    /// caller commits before encoding anything that is not the span's.
    pub fn end(&mut self, encoder: &mut GpuEncoder) {
        let Some(open) = self.open.take() else {
            return;
        };
        if let Some(earlier) = std::mem::replace(&mut encoder.span_closing, encoder.span.take()) {
            // Two spans ending in one buffer: the earlier goes without it.
            earlier.closed();
        }
        self.ended.push_back(open);
    }

    /// Hand `each` the key, frame and longest submit's GPU seconds of every
    /// finished span, oldest first. One queue runs its submits in order, so
    /// reading stops at the first unfinished span.
    pub fn poll(&mut self, mut each: impl FnMut(u64, u64, f64)) {
        while let Some(&(key, frame, index)) = self.ended.front() {
            let slot = &self.slots[index];
            if slot.pending.load(Ordering::Acquire) != 0 {
                break;
            }
            self.ended.pop_front();
            self.free.push(index);
            let longest = f64::from_bits(slot.longest.load(Ordering::Relaxed));
            if longest > 0.0 {
                each(key, frame, longest);
            }
        }
    }

    /// The frame of the oldest span still on the GPU. Every span of an
    /// earlier frame has been handed out by [`Self::poll`].
    pub fn in_flight_frame(&self) -> Option<u64> {
        self.ended.front().map(|&(_, frame, _)| frame)
    }
}
