//! Off-thread BS.1770 integrated-LUFS + LRA recompute.
//!
//! The audio thread's `LoudnessMeter` pushes closed 100 ms block `z` values
//! into `AnalyzerGuiShared::loudness_block_queue`. This worker drains that
//! queue, maintains its own block-mean-square history, and recomputes the
//! gated integrated + LRA at each update. The resulting scalars are
//! published to atomics on `AnalyzerGuiShared`; the audio thread reads the
//! integrated value back to derive DR / PLR.
//!
//! Why it's worth a dedicated thread: `compute_integrated_and_lra` is O(N)
//! where N is the number of closed 100 ms bins — 10 per second. At 1 hour
//! that's 36 000 bins × 2 passes (momentary + LRA windows) per update,
//! which previously ran every 100 ms on the audio thread. Offloading makes
//! long-session audio CPU flat regardless of session length.

use crate::AnalyzerGuiShared;
use manifold_analyzer_dsp::{compute_integrated_and_lra, IntegratedScratch};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Poll cadence when the queue is idle. Bounded by this — once new blocks
/// arrive the loop drains immediately. Matches the 100 ms block cadence so
/// worst-case latency from audio-thread push to atomic publish is ~2 × this.
const IDLE_SLEEP: Duration = Duration::from_millis(50);

/// Pre-allocated history capacity. Sized for ~30 min of session before
/// amortised Vec growth kicks in (still bounded memcpys, never audio-
/// affecting because this is the worker thread).
const PRESIZE_BINS: usize = 18_000;

pub struct LoudnessWorker {
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LoudnessWorker {
    /// Spawn the worker. Lives for the `AnalyzerGuiShared`'s lifetime
    /// (typically the plugin instance) and joins cleanly on `Drop`.
    pub fn spawn(shared: Arc<AnalyzerGuiShared>) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread = {
            let shutdown = shutdown.clone();
            thread::Builder::new()
                .name("manifold-analyzer-loudness".into())
                .spawn(move || worker_loop(shared, shutdown))
                .expect("spawn manifold-analyzer-loudness thread")
        };
        Self {
            shutdown,
            thread: Some(thread),
        }
    }
}

impl Drop for LoudnessWorker {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn worker_loop(shared: Arc<AnalyzerGuiShared>, shutdown: Arc<AtomicBool>) {
    let mut history = LoudnessHistory::new(shared.loudness_reset_epoch());
    while !shutdown.load(Ordering::Acquire) {
        history.set_epoch(shared.loudness_reset_epoch());
        let mut drained = false;
        while let Some(block) = shared.loudness_block_queue.pop() {
            history.set_epoch(shared.loudness_reset_epoch());
            drained |= history.push(block);
        }
        if !drained { thread::sleep(IDLE_SLEEP); continue; }
        let (integrated, lra) = history.compute();
        shared.publish_slow_loudness(history.epoch, integrated, lra);
    }
}

struct LoudnessHistory {
    epoch: u32,
    blocks: Vec<f32>,
    scratch: IntegratedScratch,
}
impl LoudnessHistory {
    fn new(epoch: u32) -> Self {
        Self { epoch, blocks: Vec::with_capacity(PRESIZE_BINS), scratch: IntegratedScratch::default() }
    }
    fn set_epoch(&mut self, epoch: u32) {
        if self.epoch != epoch { self.epoch = epoch; self.blocks.clear(); }
    }
    fn push(&mut self, block: manifold_analyzer_dsp::LoudnessBlock) -> bool {
        if block.generation != self.epoch { return false; }
        self.blocks.push(block.mean_square);
        true
    }
    fn compute(&mut self) -> (f32, f32) {
        let (integrated, lra) = compute_integrated_and_lra(&self.blocks, &mut self.scratch);
        (integrated.unwrap_or(-120.0), lra.unwrap_or(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_analyzer_dsp::LoudnessBlock;
    #[test]
    fn reset_rejects_pending_old_blocks_and_old_results() {
        let shared = AnalyzerGuiShared::new(48000.0, 4096);
        let mut history = LoudnessHistory::new(0);
        for _ in 0..10 { history.push(LoudnessBlock { generation: 0, mean_square: 1.0 }); }
        let old = history.compute();
        shared.request_loudness_reset();
        history.set_epoch(shared.loudness_reset_epoch());
        assert!(!history.push(LoudnessBlock { generation: 0, mean_square: 1.0 }));
        shared.publish_slow_loudness(0, old.0, old.1);
        assert_eq!(shared.integrated_lufs(), -120.0);
        assert_eq!(shared.loudness().lra_lu, 0.0);
        for _ in 0..10 { history.push(LoudnessBlock { generation: 1, mean_square: 0.01 }); }
        let fresh = history.compute();
        shared.publish_slow_loudness(1, fresh.0, fresh.1);
        assert!((shared.integrated_lufs() + 20.691).abs() < 0.001);
        assert_eq!(history.blocks.len(), 10);
    }
}
