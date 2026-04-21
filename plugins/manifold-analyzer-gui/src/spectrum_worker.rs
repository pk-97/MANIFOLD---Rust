//! Dedicated CQT worker thread.
//!
//! Audio thread pushes raw samples into the `SampleRing`; this thread drains
//! them, runs the CQT + synchrosqueezing pipeline, and pushes completed
//! columns into a bounded SPSC ring that the GUI drains each redraw.
//!
//! Why a separate thread:
//! 1. CQT is ~2.5 ms per hop and the GUI used to run it inline. After a
//!    stall, the ring could buffer ~1 s of audio → 250 hops → a visible
//!    freeze. The worker decouples CQT cadence from redraw cadence.
//! 2. The worker's natural rate is 187 Hz (5.33 ms / hop). Redraws at 60 Hz
//!    no longer gate analysis cadence, so the spectrogram stays current even
//!    when the GUI is throttled (window drag, DPI change, inactive tab).
//!
//! Threading contract: worker uses only atomics + lock-free rings + one
//! parking_lot mutex for the config snapshot. The audio thread stays
//! entirely out of this file — its only duty is pushing into `SampleRing`.

use crate::cqt::{CqtComplex, CqtTransform};
use crate::gpu_cqt::GpuCqt;
use crate::AnalyzerGuiShared;
use crossbeam_queue::ArrayQueue;
use manifold_gpu::GpuDevice;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Sentinel dB for "silent" bins (no content / cleared history). Well
/// below `SPECTROGRAM_DB_MIN` so the shader's colormap renders it as
/// black. Matches `spectrum_gpu::FLOOR_DB`; kept local so the worker
/// doesn't depend on the renderer module.
const WORKER_FLOOR_DB: f32 = -140.0;

/// Up to N hops per drain. Stall recovery: if the sample ring backed up
/// while the audio thread kept pushing (e.g. worker was briefly parked by
/// OS scheduling), we still only CQT the most recent N hops. Rolling ring
/// still takes every sample so the first emitted CQT sees a valid window.
const MAX_HOPS_PER_DRAIN: usize = 32;

/// Idle sleep between ring polls when no samples are pending. Keeps worker
/// CPU at near-zero when playback is stopped; wake-up latency is bounded
/// by this interval in the worst case.
const IDLE_SLEEP: Duration = Duration::from_millis(2);

/// Completed CQT column pushed from worker → GUI. The GUI writes `data` into
/// the history storage buffer at `col_idx`, optionally clearing the entire
/// history first (after a tempo / sync config change invalidates the grid).
pub struct CqtColumnMsg {
    pub col_idx: u32,
    /// dB per log bin. Length == `CqtWorker::cqt_num_bins()`.
    pub data: Vec<f32>,
    /// When true, consumer must clear the full spectrogram history buffer
    /// before applying this column. Issued on sync toggle / tempo change /
    /// sync window width change.
    pub clear_history_before: bool,
}

/// Snapshot of GUI-controlled knobs the worker needs each hop. Updated via
/// `CqtWorker::post_config`; the worker reads it behind a tiny uncontested
/// `parking_lot::Mutex` once per hop.
#[derive(Copy, Clone, Debug)]
pub struct WorkerConfig {
    pub sync_enabled: bool,
    /// 0.0 means "unknown" (host didn't provide one).
    pub bpm: f32,
    /// NaN means "unknown".
    pub beat_pos: f64,
    pub beats_per_window: f32,
    pub synchrosqueeze: bool,
    pub coherence: bool,
    pub synchro_gate_db: f32,
}

impl WorkerConfig {
    pub const OFF: Self = Self {
        sync_enabled: false,
        bpm: 0.0,
        beat_pos: f64::NAN,
        beats_per_window: 0.0,
        synchrosqueeze: false,
        coherence: false,
        synchro_gate_db: -75.0,
    };
}

/// Parameters describing the CQT to build. Decoupled from `spectrum_gpu`
/// constants so the worker can be unit-tested with smaller transforms.
#[derive(Clone, Copy, Debug)]
pub struct CqtBuildParams {
    pub n_fft: usize,
    pub fmin: f32,
    pub fmax: f32,
    pub bpo: usize,
    pub gamma_lo: f32,
    pub gamma_hi: f32,
    pub gamma_transition: f32,
    pub min_kernel_len: usize,
    pub causal: bool,
    pub threshold_rel: f32,
    pub hop_samples: usize,
    pub history_cols: u32,
}

pub struct CqtWorker {
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    column_ring: Arc<ArrayQueue<CqtColumnMsg>>,
    config: Arc<Mutex<WorkerConfig>>,
    cqt_num_bins: usize,
}

impl CqtWorker {
    /// Build the CQT on the calling thread (so the ~500 ms kernel
    /// construction happens on the GUI thread during first paint, not on
    /// the worker where it would delay first output) and spawn the worker.
    ///
    /// `device` is the same `GpuDevice` the renderer uses; both the GUI
    /// thread's spectrum renderer and this worker hand buffers to the
    /// underlying MTLDevice's shared command queue (thread-safe per
    /// Apple).
    pub fn spawn(
        sample_rate: f32,
        shared: Arc<AnalyzerGuiShared>,
        device: Arc<GpuDevice>,
        params: CqtBuildParams,
    ) -> Self {
        let cqt = CqtTransform::new(
            sample_rate,
            params.n_fft,
            params.fmin,
            params.fmax,
            params.bpo,
            params.gamma_lo,
            params.gamma_hi,
            params.gamma_transition,
            params.min_kernel_len,
            params.causal,
            params.threshold_rel,
        );
        let cqt_num_bins = cqt.num_bins();

        // Build the GPU side of the pipeline now so the worker thread
        // starts processing hops immediately. Kernel buffer uploads +
        // MPSGraph compile happen here.
        let gpu_cqt = GpuCqt::new(&device, &cqt);

        // Ring sized so the GUI can fall ~1 s behind at 187 hops/sec
        // without loss. Beyond that the producer's `force_push` wins —
        // oldest columns drop, matching the "visible gap, never a
        // freeze" behaviour of the stall-recovery cap.
        let column_ring = Arc::new(ArrayQueue::new(256));
        let config = Arc::new(Mutex::new(WorkerConfig::OFF));
        let shutdown = Arc::new(AtomicBool::new(false));

        let thread = {
            let column_ring = column_ring.clone();
            let config = config.clone();
            let shutdown = shutdown.clone();
            let device = device.clone();
            thread::Builder::new()
                .name("manifold-analyzer-cqt".into())
                .spawn(move || {
                    let mut state = WorkerState::new(gpu_cqt, params, sample_rate);
                    worker_loop(&mut state, device, shared, column_ring, config, shutdown);
                })
                .expect("spawn manifold-analyzer-cqt thread")
        };

        Self {
            shutdown,
            thread: Some(thread),
            column_ring,
            config,
            cqt_num_bins,
        }
    }

    /// Publish a new config snapshot. Cheap — only the worker reads it.
    pub fn post_config(&self, cfg: WorkerConfig) {
        *self.config.lock() = cfg;
    }

    /// Drain all available completed columns. Callback is invoked in FIFO
    /// order. Returns the number drained.
    pub fn drain_columns<F: FnMut(CqtColumnMsg)>(&self, mut f: F) -> usize {
        let mut count = 0;
        while let Some(col) = self.column_ring.pop() {
            f(col);
            count += 1;
        }
        count
    }

    pub fn cqt_num_bins(&self) -> usize {
        self.cqt_num_bins
    }
}

impl Drop for CqtWorker {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ─── Worker-internal state ──────────────────────────────────────────────

struct WorkerState {
    n_fft: usize,
    hop: usize,
    history_cols: u32,
    cqt_num_bins: usize,
    sample_rate: f32,

    /// GPU pipeline: R2C FFT + sparse CSR mat-vec in one command buffer.
    /// Replaced the CPU `CqtTransform::process_complex` path — that CPU
    /// transform still exists (kernel construction happens on it at
    /// plan time) but no longer runs per hop.
    gpu_cqt: GpuCqt,
    rolling: Vec<f32>,
    rolling_head: usize,
    samples_since_last_hop: usize,
    scratch_audio: Vec<f32>,

    cqt_complex: Vec<CqtComplex<f32>>,
    prev_cqt_complex: Vec<CqtComplex<f32>>,
    prev2_cqt_complex: Vec<CqtComplex<f32>>,
    have_prev_cqt: bool,
    have_prev2_cqt: bool,
    synchro_power_scratch: Vec<f32>,
    cqt_out: Vec<f32>,
    prev_cqt_out: Vec<f32>,
    have_prev_cqt_out: bool,

    internal_beat_pos: f64,
    have_internal_beat: bool,
    write_col: u32,
    last_applied_cfg: WorkerConfig,
    /// Pending-clear flag — set when config change invalidates history.
    /// Consumed on the next emitted column so the GUI clears first, then
    /// writes the fresh data.
    clear_pending: bool,
}

impl WorkerState {
    fn new(gpu_cqt: GpuCqt, params: CqtBuildParams, sample_rate: f32) -> Self {
        let cqt_num_bins = gpu_cqt.num_bins();
        Self {
            n_fft: params.n_fft,
            hop: params.hop_samples,
            history_cols: params.history_cols,
            cqt_num_bins,
            sample_rate,
            gpu_cqt,
            rolling: vec![0.0; params.n_fft],
            rolling_head: 0,
            samples_since_last_hop: 0,
            scratch_audio: vec![0.0; params.n_fft],
            cqt_complex: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            prev_cqt_complex: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            prev2_cqt_complex: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            have_prev_cqt: false,
            have_prev2_cqt: false,
            synchro_power_scratch: vec![0.0; cqt_num_bins],
            cqt_out: vec![WORKER_FLOOR_DB; cqt_num_bins],
            prev_cqt_out: vec![WORKER_FLOOR_DB; cqt_num_bins],
            have_prev_cqt_out: false,
            internal_beat_pos: 0.0,
            have_internal_beat: false,
            write_col: params.history_cols - 1,
            last_applied_cfg: WorkerConfig::OFF,
            clear_pending: false,
        }
    }

    /// Apply a new config snapshot, detecting changes that invalidate the
    /// current grid / beat clock.
    fn apply_config(&mut self, cfg: WorkerConfig) {
        let changed = cfg.sync_enabled != self.last_applied_cfg.sync_enabled
            || (cfg.sync_enabled
                && ((cfg.bpm - self.last_applied_cfg.bpm).abs() > 0.05
                    || (cfg.beats_per_window - self.last_applied_cfg.beats_per_window).abs()
                        > 1e-3));
        if changed {
            self.clear_pending = true;
            self.write_col = self.history_cols - 1;
            self.have_internal_beat = false;
            self.have_prev_cqt_out = false;
        }
        if cfg.sync_enabled {
            // Re-anchor to the host's beat position when we don't have a
            // clock yet or we've drifted more than half a window (seek /
            // loop jump). Small steady drift is invisible over seconds.
            if !cfg.beat_pos.is_nan() {
                let threshold = (cfg.beats_per_window as f64 * 0.5).max(0.1);
                let need_anchor = !self.have_internal_beat
                    || (self.internal_beat_pos - cfg.beat_pos).abs() > threshold;
                if need_anchor {
                    self.internal_beat_pos = cfg.beat_pos;
                    self.have_internal_beat = true;
                }
            }
        } else {
            self.have_internal_beat = false;
        }
        self.last_applied_cfg = cfg;
    }

    /// Process drained samples. Runs CQT for the most recent hops, skipping
    /// older ones when the ring backed up. Pushes completed columns into the
    /// output ring (dropping oldest on overflow — matches the "gap, not
    /// freeze" policy).
    fn process(
        &mut self,
        device: &GpuDevice,
        samples: &[f32],
        cfg: &WorkerConfig,
        ring: &ArrayQueue<CqtColumnMsg>,
    ) {
        let beats_per_hop = if cfg.bpm > 0.0 {
            self.hop as f64 / self.sample_rate as f64 * cfg.bpm as f64 / 60.0
        } else {
            0.0
        };

        let pending = self.samples_since_last_hop + samples.len();
        let total_hops = pending / self.hop;
        let skip_hops = total_hops.saturating_sub(MAX_HOPS_PER_DRAIN);
        let mut hops_seen: usize = 0;

        for &s in samples {
            self.rolling[self.rolling_head] = s;
            self.rolling_head = (self.rolling_head + 1) % self.n_fft;
            self.samples_since_last_hop += 1;
            if self.samples_since_last_hop >= self.hop {
                self.samples_since_last_hop = 0;
                hops_seen += 1;

                if hops_seen <= skip_hops {
                    // Skip expensive CQT but keep the beat clock consistent.
                    self.internal_beat_pos += beats_per_hop;
                    continue;
                }

                self.emit_column(device, cfg, ring);
                self.internal_beat_pos += beats_per_hop;
            }
        }
    }

    fn emit_column(
        &mut self,
        device: &GpuDevice,
        cfg: &WorkerConfig,
        ring: &ArrayQueue<CqtColumnMsg>,
    ) {
        // Copy rolling ring into oldest→newest linear order.
        let head = self.rolling_head;
        let (tail, front) = self.rolling.split_at(head);
        self.scratch_audio[..front.len()].copy_from_slice(front);
        self.scratch_audio[front.len()..].copy_from_slice(tail);

        // GPU CQT: uploads scratch_audio → runs FFT + sparse mat-vec →
        // writes result into `cqt_complex`. Synchronous so the CPU-side
        // synchrosqueezing + column push can run on the same thread
        // without waiting on a fence.
        self.gpu_cqt
            .process(device, &self.scratch_audio, &mut self.cqt_complex);

        if cfg.synchrosqueeze && self.have_prev_cqt {
            self.synchrosqueeze(cfg);
        } else {
            for (dst, c) in self.cqt_out.iter_mut().zip(self.cqt_complex.iter()) {
                let p = c.norm_sqr();
                *dst = if p > 1e-20 {
                    10.0 * p.log10()
                } else {
                    WORKER_FLOOR_DB
                };
            }
        }

        // Shift the phase history. Done unconditionally so synchrosqueezing
        // toggled on later immediately has valid context.
        if self.have_prev_cqt {
            self.prev2_cqt_complex
                .copy_from_slice(&self.prev_cqt_complex);
            self.have_prev2_cqt = true;
        }
        self.prev_cqt_complex.copy_from_slice(&self.cqt_complex);
        self.have_prev_cqt = true;

        // Decide target column(s).
        let prev_col = self.write_col;
        let beat_at_hop = self.internal_beat_pos;
        let new_col = if cfg.sync_enabled && cfg.beats_per_window > 0.0 {
            let t = beat_at_hop / cfg.beats_per_window as f64;
            let frac = t - t.floor();
            let col = (frac * self.history_cols as f64) as u32;
            col.min(self.history_cols - 1)
        } else {
            (self.write_col + 1) % self.history_cols
        };
        self.write_col = new_col;

        let forward = (new_col + self.history_cols - prev_col) % self.history_cols;
        let max_forward =
            if cfg.sync_enabled && cfg.beats_per_window > 0.0 && cfg.bpm > 0.0 {
                let hop_in_beats = self.hop as f64 / self.sample_rate as f64
                    * cfg.bpm as f64
                    / 60.0;
                let cph = (hop_in_beats / cfg.beats_per_window as f64
                    * self.history_cols as f64)
                    .ceil() as u32;
                (cph * 3).max(4)
            } else {
                1
            };
        let (start_col, fill_count) = if forward == 0 || forward > max_forward {
            (new_col, 1_u32)
        } else {
            ((prev_col + 1) % self.history_cols, forward)
        };

        // Emit one ColumnMsg per screen column. Single path (no lerp) when
        // fill_count == 1 OR no previous frame exists.
        let clear_first = std::mem::take(&mut self.clear_pending);
        if fill_count == 1 || !self.have_prev_cqt_out {
            if fill_count == 1 {
                Self::push_column(ring, start_col, &self.cqt_out, clear_first);
            } else {
                // Hold current across the span (first hop after a mode change).
                let mut c = start_col;
                for i in 0..fill_count {
                    let clear = clear_first && i == 0;
                    Self::push_column(ring, c, &self.cqt_out, clear);
                    c = (c + 1) % self.history_cols;
                }
            }
        } else {
            let mut c = start_col;
            let denom = fill_count as f32;
            // Lerp in the POWER domain, not dB. dB-lerp is a geometric mean
            // of powers — energy-losing by up to ~3 dB at the midpoint and
            // it dims sustained content. Power-domain lerp preserves the
            // arithmetic mean of energy across the span, which is what the
            // CQT magnitude actually measures. Converted back to dB for
            // storage at the end.
            for i in 0..fill_count {
                let t = (i + 1) as f32 / denom;
                let one_minus_t = 1.0 - t;
                let data: Vec<f32> = self
                    .prev_cqt_out
                    .iter()
                    .zip(self.cqt_out.iter())
                    .map(|(&prev_db, &cur_db)| {
                        let p_prev = 10.0_f32.powf(prev_db * 0.1);
                        let p_cur = 10.0_f32.powf(cur_db * 0.1);
                        let p = p_prev * one_minus_t + p_cur * t;
                        if p > 1e-20 {
                            10.0 * p.log10()
                        } else {
                            WORKER_FLOOR_DB
                        }
                    })
                    .collect();
                let clear = clear_first && i == 0;
                force_push(
                    ring,
                    CqtColumnMsg {
                        col_idx: c,
                        data,
                        clear_history_before: clear,
                    },
                );
                c = (c + 1) % self.history_cols;
            }
        }

        self.prev_cqt_out.copy_from_slice(&self.cqt_out);
        self.have_prev_cqt_out = true;
    }

    fn push_column(
        ring: &ArrayQueue<CqtColumnMsg>,
        col_idx: u32,
        data: &[f32],
        clear_history_before: bool,
    ) {
        force_push(
            ring,
            CqtColumnMsg {
                col_idx,
                data: data.to_vec(),
                clear_history_before,
            },
        );
    }

    fn synchrosqueeze(&mut self, cfg: &WorkerConfig) {
        let center_freqs = self.gpu_cqt.center_freqs();
        let bandwidths = self.gpu_cqt.bandwidths_hz();
        let num_bins = self.cqt_num_bins;
        let hop = self.hop as f32;
        let two_pi = std::f32::consts::TAU;
        let freq_per_rad_per_hop = self.sample_rate / (two_pi * hop);
        let hop_over_sr = hop / self.sample_rate;
        let mag_gate_power = 10.0_f32.powf(cfg.synchro_gate_db * 0.1);
        let unambiguous_hz = self.sample_rate / (2.0 * hop) * 0.8;
        let fmin = center_freqs[0];
        let log2_ratio = if num_bins > 1 {
            (center_freqs[1] / center_freqs[0]).log2()
        } else {
            1.0 / 24.0
        };
        let inv_log2_ratio = 1.0 / log2_ratio;

        self.synchro_power_scratch.fill(0.0);

        for k in 0..num_bins {
            let x_curr = self.cqt_complex[k];
            let x_prev = self.prev_cqt_complex[k];
            let x_prev2 = self.prev2_cqt_complex[k];
            let power = x_curr.norm_sqr();
            let prev_power = x_prev.norm_sqr();
            let prev2_power = x_prev2.norm_sqr();
            if power < mag_gate_power || prev_power < mag_gate_power {
                continue;
            }
            let f_k = center_freqs[k];
            let expected = two_pi * f_k * hop_over_sr;
            let raw_dev_now = x_curr.arg() - x_prev.arg() - expected;
            let wrapped_now = raw_dev_now - two_pi * (raw_dev_now / two_pi).round();
            let if_now = f_k + wrapped_now * freq_per_rad_per_hop;

            if if_now <= 0.0 {
                continue;
            }

            let if_deviation = (if_now - f_k).abs();
            let gate_hz = bandwidths[k].min(unambiguous_hz);
            if if_deviation > gate_hz {
                continue;
            }

            if cfg.coherence && prev2_power >= mag_gate_power {
                let raw_dev_prev = x_prev.arg() - x_prev2.arg() - expected;
                let wrapped_prev =
                    raw_dev_prev - two_pi * (raw_dev_prev / two_pi).round();
                let if_prev = f_k + wrapped_prev * freq_per_rad_per_hop;
                let coherence_threshold = bandwidths[k] * 0.5;
                if (if_now - if_prev).abs() > coherence_threshold {
                    continue;
                }
            }

            let log_bin_f = (if_now / fmin).log2() * inv_log2_ratio;
            if log_bin_f.is_nan() || log_bin_f < 0.0 {
                continue;
            }
            let lo = log_bin_f as usize;
            if lo >= num_bins {
                continue;
            }
            let frac = log_bin_f - lo as f32;
            self.synchro_power_scratch[lo] += power * (1.0 - frac);
            let hi = lo + 1;
            if hi < num_bins {
                self.synchro_power_scratch[hi] += power * frac;
            }
        }

        for (dst, &p) in self.cqt_out.iter_mut().zip(self.synchro_power_scratch.iter()) {
            *dst = if p > 1e-20 {
                10.0 * p.log10()
            } else {
                WORKER_FLOOR_DB
            };
        }
    }
}

/// Evict-oldest push. Uses `ArrayQueue::force_push` so the worker never
/// blocks when the GUI falls behind — the oldest column drops and the
/// spectrogram shows a gap, not a stutter.
fn force_push(ring: &ArrayQueue<CqtColumnMsg>, msg: CqtColumnMsg) {
    let _ = ring.force_push(msg);
}

fn worker_loop(
    state: &mut WorkerState,
    device: Arc<GpuDevice>,
    shared: Arc<AnalyzerGuiShared>,
    column_ring: Arc<ArrayQueue<CqtColumnMsg>>,
    config: Arc<Mutex<WorkerConfig>>,
    shutdown: Arc<AtomicBool>,
) {
    // Local scratch — keeping this off WorkerState means `process` can take
    // &mut self without borrowing gymnastics around a field mid-drain.
    let mut sample_scratch: Vec<f32> = Vec::with_capacity(8192);
    while !shutdown.load(Ordering::Acquire) {
        sample_scratch.clear();
        let drained = shared.mid_sample_ring.drain_into(&mut sample_scratch);
        if drained == 0 {
            thread::sleep(IDLE_SLEEP);
            continue;
        }
        let cfg = *config.lock();
        state.apply_config(cfg);
        state.process(&device, &sample_scratch, &cfg, &column_ring);
    }
}
