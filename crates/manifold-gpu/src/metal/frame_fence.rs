//! FrameFence — GPU-completion tracking for CPU-mapped ring buffers.
//!
//! UI immediate-draw vertex rings (layer bitmap, clip content/thumb, UI
//! renderer) are `StorageModeShared` buffers recycled by a fixed ring depth.
//! Without completion tracking, a ring slot can be rewritten by a later
//! frame while an earlier frame's command buffer is still queued (GPU
//! backlog under heavy scenes), mangling the in-flight draw. `FrameFence`
//! gives ring owners a cheap "has frame N's GPU work retired yet" check so
//! they can stall a slot claim instead of racing the GPU.
//!
//! Frame numbers start at 1; `0` is reserved as the "never used" sentinel
//! and is always considered completed (a ring slot that has never been
//! claimed has nothing to wait for).
//!
//! `lag()` is primarily consumed by UI-thread admission control (skip a
//! redraw and re-present the cached offscreen instead of encoding new ring
//! work when the GPU is badly behind) — `guard_slot`'s blocking wait is the
//! backstop for whatever admission control lets through.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::device::GpuDevice;

const WAIT_POLL_INTERVAL: Duration = Duration::from_micros(500);
const WAIT_TIMEOUT: Duration = Duration::from_millis(50);

/// Tracks how far frame encoding has progressed (`encoded`) versus how far
/// the GPU has actually retired (`completed`). One instance is shared
/// (`Arc`) across all UI ring owners for a given window/content pipeline.
pub struct FrameFence {
    encoded: AtomicU64,
    completed: Arc<AtomicU64>,
}

impl FrameFence {
    pub fn new() -> Self {
        Self {
            encoded: AtomicU64::new(0),
            completed: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Advance to the next frame number. Call once per frame, before any
    /// ring-owning encoder work is recorded.
    pub fn advance(&self) -> u64 {
        self.encoded.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// The most recently advanced frame number (0 if `advance` was never
    /// called).
    pub fn current_frame(&self) -> u64 {
        self.encoded.load(Ordering::SeqCst)
    }

    /// Whether the GPU has retired everything encoded through `frame`.
    /// Frame `0` (the "never claimed" sentinel) is always completed.
    pub fn is_completed(&self, frame: u64) -> bool {
        frame == 0 || self.completed.load(Ordering::SeqCst) >= frame
    }

    /// Most recent frame the GPU has confirmed retired. Diagnostic use only
    /// (ring-owner stall logging) — correctness checks go through
    /// `is_completed`/`wait_for`, which read the same counter atomically.
    pub fn completed_frame(&self) -> u64 {
        self.completed.load(Ordering::SeqCst)
    }

    /// How many frames of encoded-but-not-yet-retired GPU work are
    /// outstanding. Used by admission control to decide whether to skip a
    /// UI redraw rather than let ring owners stall mid-encode.
    pub fn lag(&self) -> u64 {
        self.current_frame()
            .saturating_sub(self.completed_frame())
    }

    /// Block (via short sleeps, not a busy spin) until `frame` is
    /// completed or `WAIT_TIMEOUT` elapses. Returns whether it completed.
    pub fn wait_for(&self, frame: u64) -> bool {
        if self.is_completed(frame) {
            return true;
        }
        let start = Instant::now();
        while start.elapsed() < WAIT_TIMEOUT {
            std::thread::sleep(WAIT_POLL_INTERVAL);
            if self.is_completed(frame) {
                return true;
            }
        }
        self.is_completed(frame)
    }

    /// Commit an empty command buffer whose completion handler marks the
    /// frame that was current at call time as retired. Metal executes
    /// command buffers on a queue in commit order, so this only fires once
    /// every UI encoder committed earlier this frame has itself completed —
    /// it needs no work of its own, just a place in the queue.
    pub fn commit_frame(&self, device: &GpuDevice) {
        let frame = self.current_frame();
        let completed = Arc::clone(&self.completed);
        let encoder = device.create_encoder("FrameFence commit");
        encoder.add_completed_handler(move || {
            completed.fetch_max(frame, Ordering::SeqCst);
        });
        encoder.commit();
    }

    #[cfg(test)]
    fn mark_completed_for_test(&self, frame: u64) {
        self.completed.store(frame, Ordering::SeqCst);
    }

    /// Ring-slot claim guard shared by every UI ring owner (layer bitmap,
    /// clip content/thumb, UI renderer): if `*stamp`'s frame hasn't retired
    /// yet, block on it (rate-limited `[frame-fence]` log on the wait path;
    /// `log::error!` if `wait_for` times out — the caller proceeds anyway
    /// rather than corrupt a frame further by refusing to draw). Always
    /// stamps `*stamp` with the current frame before returning, so the slot
    /// is guarded again on its next claim. `owner`/`slot` are for the log
    /// line only; `wait_events` is the caller's own per-struct rate-limiter
    /// counter (kept on the caller so unrelated rings don't share cadence).
    pub fn guard_slot(&self, owner: &str, slot: usize, stamp: &mut u64, wait_events: &mut u64) {
        if !self.is_completed(*stamp) {
            *wait_events += 1;
            let n = *wait_events;
            if n <= 10 || n.is_multiple_of(256) {
                log::warn!(
                    "[frame-fence] {owner}: slot {slot} stamped frame {} not yet retired \
                     (completed frame {}) — waiting",
                    *stamp,
                    self.completed_frame()
                );
            }
            // Timeout shares the caller's rate-limiter: a sustained backlog
            // would otherwise emit one error per claim, per frame — console
            // spam at exactly the moment (a live set) it hurts most.
            if !self.wait_for(*stamp) && (n <= 10 || n.is_multiple_of(256)) {
                log::error!(
                    "[frame-fence] {owner}: slot {slot} wait timed out (stamped frame {}, \
                     completed frame {}) — GPU backlog exceeds ring depth, proceeding anyway",
                    *stamp,
                    self.completed_frame()
                );
            }
        }
        *stamp = self.current_frame();
    }
}

impl Default for FrameFence {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_zero_is_the_never_used_sentinel() {
        let fence = FrameFence::new();
        assert_eq!(fence.current_frame(), 0);
        assert!(fence.is_completed(0));
    }

    #[test]
    fn advance_starts_at_one_and_increments() {
        let fence = FrameFence::new();
        assert_eq!(fence.advance(), 1);
        assert_eq!(fence.advance(), 2);
        assert_eq!(fence.advance(), 3);
        assert_eq!(fence.current_frame(), 3);
    }

    #[test]
    fn is_completed_reflects_completed_counter() {
        let fence = FrameFence::new();
        let frame = fence.advance();
        assert!(!fence.is_completed(frame));
        fence.mark_completed_for_test(frame);
        assert!(fence.is_completed(frame));
        // A later frame that hasn't been marked yet is still pending.
        let later = fence.advance();
        assert!(!fence.is_completed(later));
    }

    #[test]
    fn wait_for_returns_immediately_when_already_completed() {
        let fence = FrameFence::new();
        let frame = fence.advance();
        fence.mark_completed_for_test(frame);
        let start = Instant::now();
        assert!(fence.wait_for(frame));
        assert!(start.elapsed() < WAIT_TIMEOUT);
    }

    #[test]
    fn wait_for_times_out_when_never_completed() {
        let fence = FrameFence::new();
        let frame = fence.advance();
        assert!(!fence.wait_for(frame));
    }

    #[test]
    fn guard_slot_skips_wait_for_the_never_used_sentinel() {
        let fence = FrameFence::new();
        let _ = fence.advance();
        let mut stamp = 0u64;
        let mut wait_events = 0u64;
        let start = Instant::now();
        fence.guard_slot("Test", 0, &mut stamp, &mut wait_events);
        assert!(start.elapsed() < WAIT_TIMEOUT);
        assert_eq!(wait_events, 0);
        assert_eq!(stamp, fence.current_frame());
    }

    #[test]
    fn guard_slot_restamps_to_current_frame_when_already_retired() {
        let fence = FrameFence::new();
        let claimed = fence.advance();
        let mut stamp = claimed;
        fence.mark_completed_for_test(claimed);
        let next = fence.advance();
        let mut wait_events = 0u64;
        fence.guard_slot("Test", 0, &mut stamp, &mut wait_events);
        assert_eq!(stamp, next);
    }

    #[test]
    fn lag_is_zero_when_nothing_encoded() {
        let fence = FrameFence::new();
        assert_eq!(fence.lag(), 0);
    }

    #[test]
    fn lag_reflects_outstanding_encoded_frames() {
        let fence = FrameFence::new();
        fence.advance();
        fence.advance();
        fence.advance();
        assert_eq!(fence.current_frame(), 3);
        assert_eq!(fence.lag(), 3);
        fence.mark_completed_for_test(2);
        assert_eq!(fence.lag(), 1);
        fence.mark_completed_for_test(3);
        assert_eq!(fence.lag(), 0);
    }
}
