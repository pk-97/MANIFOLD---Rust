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
    let mut block_msq: Vec<f32> = Vec::with_capacity(PRESIZE_BINS);
    let mut scratch = IntegratedScratch::default();
    let mut last_reset_epoch = shared.loudness_reset_epoch();

    while !shutdown.load(Ordering::Acquire) {
        // Observe resets: audio thread bumps the epoch, meter.reset()
        // fires, and we match by wiping our history so the next
        // integrated/LRA recompute starts fresh.
        let epoch = shared.loudness_reset_epoch();
        if epoch != last_reset_epoch {
            block_msq.clear();
            shared.set_integrated_lufs(DSP_MIN_LUFS);
            shared.set_lra_lu(0.0);
            last_reset_epoch = epoch;
        }

        let mut drained = 0;
        while let Some(z) = shared.loudness_block_queue.pop() {
            block_msq.push(z);
            drained += 1;
        }

        if drained == 0 {
            thread::sleep(IDLE_SLEEP);
            continue;
        }

        let (integrated, lra) = compute_integrated_and_lra(&block_msq, &mut scratch);
        if let Some(v) = integrated {
            shared.set_integrated_lufs(v);
        }
        if let Some(v) = lra {
            shared.set_lra_lu(v);
        }
    }
}

/// Mirror of the meter's `MIN_LUFS` so the worker can publish "unknown"
/// without pulling in the dsp module's private constants.
const DSP_MIN_LUFS: f32 = -120.0;
