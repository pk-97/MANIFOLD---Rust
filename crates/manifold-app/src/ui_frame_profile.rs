//! Per-frame UI profiler for the main timeline window.
//!
//! Gated behind `MANIFOLD_UI_FRAME_PROFILE=1` — near-zero cost when unset:
//! every `add`/`count`/`frame_end` short-circuits on the `enabled` flag. The
//! `Instant::now()` pairs at the call sites are unconditional (~20ns each) but
//! the recording, formatting, and printing only happen when enabled.
//!
//! Why this exists: the perf HUD reports the *total* UI frame time (the
//! frame-to-frame `dt`). When that overruns the 8.3ms / 120Hz budget — the
//! 120→77fps regression after the timeline redesign — the total alone can't
//! say *which pass* ate the time, or even whether the cost is CPU (our passes)
//! or GPU/vsync wait (present back-pressure). This attributes the frame to
//! named passes and, crucially, reports `dt − measured_cpu` so a CPU-bound
//! frame (gap ≈ 0) is distinguishable from a present/vsync-bound one (gap
//! large). See `present_all_windows` / `tick_and_render` for the call sites.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ahash::AHashMap;

/// Emit a report every this many profiled frames (~1s at 60fps).
const REPORT_EVERY: u64 = 60;

/// Fixed print order — frame-sequential, so the report reads top-to-bottom in
/// the order the passes actually run. Labels not in this list are appended.
const ORDER: &[&str] = &[
    "drain_state",
    "process_events",
    "rebuild_tree",
    "update_repaint_upload",
    "present.panel_cache",
    "present.clear_atlas_compositor",
    "present.pass4a_grid",
    "present.pass4b_clip_bodies",
    "present.pass4b_waveforms",
    "present.pass4b_thumbnails",
    "present.pass4c_panels",
    "present.pass5_overlay",
    "present.commit",
    "present.next_drawable",
    "present.blit_present",
    "present.fast_next_drawable",
    "present.fast_blit_present",
    "present_graph_editor",
];

pub(crate) struct UiFrameProfile {
    enabled: bool,
    sum: AHashMap<&'static str, Duration>,
    max: AHashMap<&'static str, Duration>,
    /// Per-frame integer counters (e.g. clips drawn) — summed, reported as avg.
    counts: AHashMap<&'static str, u64>,
    frame_total_sum: Duration,
    frame_total_max: Duration,
    /// Sum of inter-frame `dt` (the perf-HUD frame time) over the window.
    frame_dt_sum: Duration,
    frame_dt_max: Duration,
    /// Display link's live actual refresh rate (Hz) — last value in the window.
    display_hz: f64,
    frames: u64,
    /// True GPU execution time of the UI offscreen "Frame" buffer, fed async by
    /// a command-buffer completion handler (micros accumulated) + sample count.
    /// Shared with the handler thread, hence atomics behind `Arc`.
    gpu_us: Arc<AtomicU64>,
    gpu_samples: Arc<AtomicU64>,
}

impl UiFrameProfile {
    pub fn new() -> Self {
        let enabled = std::env::var("MANIFOLD_UI_FRAME_PROFILE")
            .map(|v| v != "0" && !v.is_empty())
            .unwrap_or(false);
        if enabled {
            eprintln!(
                "[ui-profile] ON — main-window frame breakdown every {REPORT_EVERY} frames. \
                 Watch 'vsync/idle wait': ≈0 ⇒ CPU-bound (fix the top pass); large ⇒ \
                 present/GPU-bound (cost is the drawable/commit, not our CPU passes)."
            );
        }
        Self {
            enabled,
            sum: AHashMap::new(),
            max: AHashMap::new(),
            counts: AHashMap::new(),
            frame_total_sum: Duration::ZERO,
            frame_total_max: Duration::ZERO,
            frame_dt_sum: Duration::ZERO,
            frame_dt_max: Duration::ZERO,
            display_hz: 0.0,
            frames: 0,
            gpu_us: Arc::new(AtomicU64::new(0)),
            gpu_samples: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Clones of the GPU-time accumulators to feed into a command-buffer
    /// completion handler (see `GpuEncoder::add_gpu_time_handler`). Returns
    /// `None` when disabled, so the caller skips the handler entirely.
    pub fn gpu_sink(&self) -> Option<(Arc<AtomicU64>, Arc<AtomicU64>)> {
        if self.enabled {
            Some((Arc::clone(&self.gpu_us), Arc::clone(&self.gpu_samples)))
        } else {
            None
        }
    }

    /// Record a pass duration under `label`.
    #[inline]
    pub fn add(&mut self, label: &'static str, d: Duration) {
        if !self.enabled {
            return;
        }
        *self.sum.entry(label).or_default() += d;
        let m = self.max.entry(label).or_default();
        if d > *m {
            *m = d;
        }
    }

    /// Record a per-frame integer count under `label` (e.g. clips drawn).
    #[inline]
    pub fn count(&mut self, label: &'static str, n: u64) {
        if !self.enabled {
            return;
        }
        *self.counts.entry(label).or_default() += n;
    }

    /// Close out a frame. `total` is the measured wall time of the frame body;
    /// `dt` is the inter-frame interval (the perf-HUD frame time); `display_hz`
    /// is the display link's live actual refresh rate. Reports and resets every
    /// `REPORT_EVERY` frames.
    pub fn frame_end(&mut self, total: Duration, dt: Duration, display_hz: f64) {
        if !self.enabled {
            return;
        }
        self.frame_total_sum += total;
        if total > self.frame_total_max {
            self.frame_total_max = total;
        }
        self.frame_dt_sum += dt;
        if dt > self.frame_dt_max {
            self.frame_dt_max = dt;
        }
        // Last writer wins — the rate is ~constant within a window.
        self.display_hz = display_hz;
        self.frames += 1;
        if self.frames >= REPORT_EVERY {
            self.report();
            self.reset();
        }
    }

    fn report(&self) {
        let n = self.frames.max(1) as f64;
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let avg = |d: Duration| ms(d) / n;

        let dt_avg = avg(self.frame_dt_sum);
        let fps = if dt_avg > 0.0 { 1000.0 / dt_avg } else { 0.0 };
        let cpu_avg = avg(self.frame_total_sum);
        // dt is the wall-clock budget per frame; the frame body uses cpu_avg of
        // it. The remainder is time the thread was NOT in the frame body —
        // vsync wait, nextDrawable back-pressure, GPU not yet done. ≈0 ⇒
        // CPU-bound; large ⇒ present/GPU-bound.
        let wait = (dt_avg - cpu_avg).max(0.0);

        // Sum of attributed passes, to expose any unmeasured CPU gap.
        let mut attributed = 0.0_f64;
        for d in self.sum.values() {
            attributed += avg(*d);
        }

        // True GPU execution time of the UI offscreen "Frame" buffer (async).
        let g_us = self.gpu_us.load(Ordering::Relaxed);
        let g_n = self.gpu_samples.load(Ordering::Relaxed);
        let gpu_avg = if g_n > 0 {
            g_us as f64 / 1000.0 / g_n as f64
        } else {
            0.0
        };

        eprintln!(
            "\n[ui-profile] {} frames | display link {:.1}Hz | dt {:.2}ms ({:.0} fps, max {:.2}) | cpu {:.2}ms (max {:.2}) | UI offscreen GPU {:.2}ms ({} samples) | vsync/idle wait {:.2}ms | budget 8.33ms@120 / 16.67ms@60",
            self.frames,
            self.display_hz,
            dt_avg,
            fps,
            ms(self.frame_dt_max),
            cpu_avg,
            ms(self.frame_total_max),
            gpu_avg,
            g_n,
            wait,
        );

        // Ordered labels first, then any stragglers not in ORDER.
        let mut labels: Vec<&str> = ORDER.iter().copied().filter(|l| self.sum.contains_key(l)).collect();
        for &label in self.sum.keys() {
            if !ORDER.contains(&label) {
                labels.push(label);
            }
        }
        for label in labels {
            let d = self.sum[label];
            let a = avg(d);
            let mx = self.max.get(label).copied().unwrap_or_default();
            let pct = if cpu_avg > 0.0 { a / cpu_avg * 100.0 } else { 0.0 };
            let cnt = self
                .counts
                .get(label)
                .map(|c| format!("  (avg {:.0}/frame)", *c as f64 / n))
                .unwrap_or_default();
            eprintln!(
                "  {label:<34} {a:>7.3}ms  {pct:>4.0}%   max {:>6.3}ms{cnt}",
                ms(mx),
            );
        }
        eprintln!(
            "  {:<34} {:>7.3}ms  {:>4.0}%   (sum of passes; gap vs cpu = unmeasured)",
            "= attributed",
            attributed,
            if cpu_avg > 0.0 { attributed / cpu_avg * 100.0 } else { 0.0 },
        );
    }

    fn reset(&mut self) {
        self.sum.clear();
        self.max.clear();
        self.counts.clear();
        self.frame_total_sum = Duration::ZERO;
        self.frame_total_max = Duration::ZERO;
        self.frame_dt_sum = Duration::ZERO;
        self.frame_dt_max = Duration::ZERO;
        self.frames = 0;
        self.gpu_us.store(0, Ordering::Relaxed);
        self.gpu_samples.store(0, Ordering::Relaxed);
    }
}
