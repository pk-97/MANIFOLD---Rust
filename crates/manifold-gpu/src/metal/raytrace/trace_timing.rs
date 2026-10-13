//! Time-budgeted trace tiling (BUG-k1n2w, RT trace hangs GPU on alpha-masked
//! foliage). Every trace tile commits as its own command buffer; a later
//! dispatch reads those buffers' GPU start and end times and sizes the next
//! tiles so one buffer stays near [`TRACE_BUFFER_TARGET_SECONDS`] and never
//! near the watchdog. Content thread only: the tracer polls retained buffers,
//! so there are no completion callbacks and no per-frame allocation.

use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCommandBuffer, MTLCommandBufferStatus};

use crate::trace_planner::{TraceTimeBudget, TraceWorkLimits};

static RESTART_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Make every tracer in the process start its next trace from the smallest
/// tiles, as after a camera cut. Export calls this before retrying a frame
/// that failed on the GPU: the retry then splits the trace into the most
/// command buffers the budget allows.
pub fn request_conservative_trace_tiles() {
    RESTART_EPOCH.fetch_add(1, Ordering::AcqRel);
}

/// What the tracer has measured, for proofs and diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TraceTimingReport {
    /// Trace command buffers whose GPU time has been read back.
    pub measured_buffers: u64,
    /// Longest measured trace buffer, seconds, over the tracer's life.
    pub longest_buffer_seconds: f64,
    /// Trace command buffers the most recent dispatch committed.
    pub last_dispatch_buffers: u32,
    /// Pixel budget the most recent dispatch planned with.
    pub last_pixels_per_tile: u64,
}

/// Dispatch shapes with their own cost estimate: the mask trace, the
/// lighting trace and subsurface, plus one spare for a quality change.
const BUDGET_SLOTS: usize = 4;
/// Buffers awaiting readback. Past this the oldest is dropped unmeasured.
const PENDING_CAPACITY: usize = 64;
/// A slot unused for this many dispatches (about two frames) is stale; using
/// it again restarts it from small tiles.
const STALE_DISPATCHES: u64 = 8;

/// A committed trace buffer awaiting its GPU times.
struct PendingBuffer {
    buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    slot: usize,
    generation: u32,
    pixels: u64,
    planned_pixels: u64,
}

// SAFETY: Metal command buffers are thread-safe for status and timestamp
// reads. Graph nodes move between threads; only the owning tracer, on one
// thread at a time, touches this handle.
unsafe impl Send for PendingBuffer {}

#[derive(Clone, Copy, Default)]
struct BudgetSlot {
    key: u64,
    used: bool,
    /// Bumped on every restart so measurements of the old shape are dropped.
    generation: u32,
    last_dispatch: u64,
    budget: TraceTimeBudget,
}

pub(crate) struct TraceTiming {
    slots: [BudgetSlot; BUDGET_SLOTS],
    pending: [Option<PendingBuffer>; PENDING_CAPACITY],
    next_pending: usize,
    dispatches: u64,
    seen_restart_epoch: u64,
    seen_fault_count: u64,
    report: TraceTimingReport,
}

impl TraceTiming {
    pub(crate) fn new() -> Self {
        Self {
            slots: [BudgetSlot::default(); BUDGET_SLOTS],
            pending: std::array::from_fn(|_| None),
            next_pending: 0,
            dispatches: 0,
            seen_restart_epoch: RESTART_EPOCH.load(Ordering::Acquire),
            seen_fault_count: super::super::gpu_fault::fault_count(),
            report: TraceTimingReport::default(),
        }
    }

    pub(crate) fn report(&self) -> TraceTimingReport {
        self.report
    }

    /// Every slot starts again from small tiles; in-flight measurements of
    /// the old state are discarded.
    pub(crate) fn restart_all(&mut self) {
        for slot in &mut self.slots {
            slot.budget.restart();
            slot.generation = slot.generation.wrapping_add(1);
        }
    }

    /// Read back finished buffers, honour restart requests, and pick the
    /// slot for a dispatch of shape `key`. Returns the slot and its limits.
    pub(crate) fn begin(&mut self, key: u64) -> (usize, TraceWorkLimits) {
        let epoch = RESTART_EPOCH.load(Ordering::Acquire);
        let faults = super::super::gpu_fault::fault_count();
        if epoch != self.seen_restart_epoch || faults != self.seen_fault_count {
            self.seen_restart_epoch = epoch;
            self.seen_fault_count = faults;
            self.restart_all();
        }
        self.poll();
        let now = self.dispatches;
        self.dispatches += 1;
        let index = match self.slots.iter().position(|s| s.used && s.key == key) {
            Some(index) => {
                let slot = &mut self.slots[index];
                if now - slot.last_dispatch > STALE_DISPATCHES {
                    slot.budget.restart();
                    slot.generation = slot.generation.wrapping_add(1);
                }
                index
            }
            None => {
                let index = self.slots.iter().position(|s| !s.used).unwrap_or_else(|| {
                    (0..BUDGET_SLOTS).min_by_key(|&i| self.slots[i].last_dispatch).unwrap_or(0)
                });
                let slot = &mut self.slots[index];
                slot.key = key;
                slot.used = true;
                slot.budget.restart();
                slot.generation = slot.generation.wrapping_add(1);
                index
            }
        };
        self.slots[index].last_dispatch = now;
        let limits = self.slots[index].budget.limits();
        self.report.last_pixels_per_tile = limits.max_pixels_per_tile;
        self.report.last_dispatch_buffers = 0;
        (index, limits)
    }

    /// Remember one committed trace buffer of `pixels` traced pixels.
    pub(crate) fn record(
        &mut self,
        slot: usize,
        buffer: &Retained<ProtocolObject<dyn MTLCommandBuffer>>,
        pixels: u64,
        planned_pixels: u64,
    ) {
        self.report.last_dispatch_buffers += 1;
        self.pending[self.next_pending] = Some(PendingBuffer {
            buffer: buffer.clone(),
            slot,
            generation: self.slots[slot].generation,
            pixels,
            planned_pixels,
        });
        self.next_pending = (self.next_pending + 1) % PENDING_CAPACITY;
    }

    /// Fold every finished buffer into its slot's budget. A failed trace
    /// buffer (fault or watchdog) restarts its slot from small tiles.
    pub(crate) fn poll(&mut self) {
        let mut worst_per_pixel = [0.0f64; BUDGET_SLOTS];
        let mut longest = [0.0f64; BUDGET_SLOTS];
        for entry in &mut self.pending {
            let Some(pending) = entry else { continue };
            let status = unsafe { pending.buffer.status() };
            if status != MTLCommandBufferStatus::Completed && status != MTLCommandBufferStatus::Error {
                continue;
            }
            let pending = entry.take().expect("checked above");
            let slot = &mut self.slots[pending.slot];
            if slot.generation != pending.generation {
                continue;
            }
            if status == MTLCommandBufferStatus::Error {
                slot.budget.restart();
                slot.generation = slot.generation.wrapping_add(1);
                worst_per_pixel[pending.slot] = 0.0;
                longest[pending.slot] = 0.0;
                continue;
            }
            let seconds = unsafe { pending.buffer.GPUEndTime() - pending.buffer.GPUStartTime() };
            if !(seconds.is_finite() && seconds > 0.0) || pending.pixels == 0 {
                continue;
            }
            self.report.measured_buffers += 1;
            self.report.longest_buffer_seconds = self.report.longest_buffer_seconds.max(seconds);
            longest[pending.slot] = longest[pending.slot].max(seconds);
            // A short edge tile is mostly fixed launch cost; its per-pixel
            // rate would shrink every tile for nothing.
            if pending.pixels.saturating_mul(4) >= pending.planned_pixels {
                worst_per_pixel[pending.slot] =
                    worst_per_pixel[pending.slot].max(seconds / pending.pixels as f64);
            }
        }
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if worst_per_pixel[index] > 0.0 {
                slot.budget.observe(worst_per_pixel[index], longest[index]);
            } else if longest[index] > crate::trace_planner::TRACE_BUFFER_CEILING_SECONDS {
                slot.budget.restart();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace_planner::{TRACE_RESTART_PIXELS_PER_TILE, DEFAULT_TRACE_WORK_LIMITS};

    /// The restart request is process-wide; these tests take turns so one
    /// test's request cannot land inside another's assertions.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn timing() -> (std::sync::MutexGuard<'static, ()>, TraceTiming) {
        let guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
        (guard, TraceTiming::new())
    }

    fn grown(timing: &mut TraceTiming, slot: usize) {
        timing.slots[slot].budget.observe(1e-12, 1e-6);
        timing.slots[slot].budget.observe(1e-12, 1e-6);
    }

    #[test]
    fn shapes_keep_separate_budgets_and_restarts_reach_every_one() {
        let (_serial, mut timing) = timing();
        let (mask, limits) = timing.begin(1);
        assert_eq!(limits.max_pixels_per_tile, TRACE_RESTART_PIXELS_PER_TILE);
        let (lighting, _) = timing.begin(2);
        assert_ne!(mask, lighting);
        grown(&mut timing, mask);
        grown(&mut timing, lighting);
        assert_eq!(timing.begin(1), (mask, DEFAULT_TRACE_WORK_LIMITS));
        timing.restart_all();
        assert_eq!(timing.begin(2).1.max_pixels_per_tile, TRACE_RESTART_PIXELS_PER_TILE);
        assert_eq!(timing.begin(1).1.max_pixels_per_tile, TRACE_RESTART_PIXELS_PER_TILE);
    }

    #[test]
    fn global_request_and_stale_shapes_restart_from_small_tiles() {
        let (_serial, mut timing) = timing();
        let (slot, _) = timing.begin(7);
        grown(&mut timing, slot);
        request_conservative_trace_tiles();
        assert_eq!(timing.begin(7).1.max_pixels_per_tile, TRACE_RESTART_PIXELS_PER_TILE);
        grown(&mut timing, slot);
        for key in 0..STALE_DISPATCHES + 1 {
            timing.begin(100 + key % 2);
        }
        assert_eq!(timing.begin(7).1.max_pixels_per_tile, TRACE_RESTART_PIXELS_PER_TILE,
            "a shape unused for over STALE_DISPATCHES dispatches starts small again");
    }

    #[test]
    fn a_new_shape_evicts_the_least_recently_used_slot() {
        let (_serial, mut timing) = timing();
        let slots: Vec<usize> = (0..BUDGET_SLOTS as u64).map(|key| timing.begin(key).0).collect();
        let (evicting, limits) = timing.begin(99);
        assert_eq!(evicting, slots[0]);
        assert_eq!(limits.max_pixels_per_tile, TRACE_RESTART_PIXELS_PER_TILE);
    }
}
