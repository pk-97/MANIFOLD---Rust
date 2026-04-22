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
use crate::{AnalyzerGuiShared, SpectrogramSource};
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
/// the primary history storage buffer at `col_idx`. When `data2` is
/// `Some`, the renderer is in `LeftRight` mode and should also write the
/// secondary buffer — `data` carries Left, `data2` carries Right (top
/// half / bottom half of the split spectrogram, respectively).
pub struct CqtColumnMsg {
    pub col_idx: u32,
    /// dB per log bin. Length == `CqtWorker::cqt_num_bins()`.
    /// In Mid/Side modes this is the full single-channel spectrogram
    /// column; in L+R mode this is the Left channel.
    pub data: Vec<f32>,
    /// Right channel data in L+R mode. `None` in Mid/Side modes (only
    /// the primary buffer is updated). Same length as `data` when set.
    pub data2: Option<Vec<f32>>,
    /// When true, consumer must clear the full spectrogram history buffer
    /// (both primary and secondary) before applying this column. Issued
    /// on sync toggle / tempo change / sync window width change / source
    /// switch.
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
    /// Which channel(s) feed the spectrogram. Mid/Side route a single
    /// derived stream through one CQT pass; L+R runs two passes per hop
    /// (one per channel) and emits paired column data.
    pub source: SpectrogramSource,
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
        source: SpectrogramSource::Mid,
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

/// Per-channel CQT state. Mid/Side modes use only the primary channel;
/// L+R mode uses both. Synchrosqueezing keeps its own phase history per
/// channel so toggling it on mid-stream picks up valid context for both
/// halves of the L+R split immediately.
struct ChannelState {
    rolling: Vec<f32>,
    rolling_head: usize,
    scratch_audio: Vec<f32>,
    cqt_complex: Vec<CqtComplex<f32>>,
    prev_cqt_complex: Vec<CqtComplex<f32>>,
    prev2_cqt_complex: Vec<CqtComplex<f32>>,
    have_prev_cqt: bool,
    have_prev2_cqt: bool,
    cqt_out: Vec<f32>,
    prev_cqt_out: Vec<f32>,
    have_prev_cqt_out: bool,
}

impl ChannelState {
    fn new(n_fft: usize, cqt_num_bins: usize) -> Self {
        Self {
            rolling: vec![0.0; n_fft],
            rolling_head: 0,
            scratch_audio: vec![0.0; n_fft],
            cqt_complex: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            prev_cqt_complex: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            prev2_cqt_complex: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            have_prev_cqt: false,
            have_prev2_cqt: false,
            cqt_out: vec![WORKER_FLOOR_DB; cqt_num_bins],
            prev_cqt_out: vec![WORKER_FLOOR_DB; cqt_num_bins],
            have_prev_cqt_out: false,
        }
    }

    fn reset_history(&mut self) {
        self.rolling.fill(0.0);
        self.rolling_head = 0;
        self.have_prev_cqt = false;
        self.have_prev2_cqt = false;
        self.have_prev_cqt_out = false;
    }
}

struct WorkerState {
    n_fft: usize,
    hop: usize,
    history_cols: u32,
    cqt_num_bins: usize,
    sample_rate: f32,

    /// GPU pipeline: R2C FFT + sparse CSR mat-vec in one command buffer.
    /// Shared between primary and secondary channels — `process` only
    /// reads the input slice and writes the output slice, so back-to-
    /// back calls with different buffers are safe (no leftover scratch
    /// state between passes).
    gpu_cqt: GpuCqt,
    primary: ChannelState,
    /// Secondary channel state. Allocated up front so switching to L+R
    /// mode at runtime never has to allocate on the worker thread.
    secondary: ChannelState,
    samples_since_last_hop: usize,
    synchro_power_scratch: Vec<f32>,

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
            primary: ChannelState::new(params.n_fft, cqt_num_bins),
            secondary: ChannelState::new(params.n_fft, cqt_num_bins),
            samples_since_last_hop: 0,
            synchro_power_scratch: vec![0.0; cqt_num_bins],
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
        let sync_changed = cfg.sync_enabled != self.last_applied_cfg.sync_enabled
            || (cfg.sync_enabled
                && ((cfg.bpm - self.last_applied_cfg.bpm).abs() > 0.05
                    || (cfg.beats_per_window - self.last_applied_cfg.beats_per_window).abs()
                        > 1e-3));
        let source_changed = cfg.source != self.last_applied_cfg.source;
        if sync_changed || source_changed {
            self.clear_pending = true;
            self.write_col = self.history_cols - 1;
            self.have_internal_beat = false;
            // A source switch swaps which channel is feeding the rolling
            // window, so the previous frame's CQT is no longer comparable
            // — reset per-channel phase + dB history on both buffers so
            // synchrosqueezing and power-domain lerp restart cleanly.
            self.primary.reset_history();
            self.secondary.reset_history();
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

    /// Process drained samples. `left` and `right` must be the same
    /// length (the audio thread pushes both rings in lockstep). Runs CQT
    /// for the most recent hops, skipping older ones when the rings
    /// backed up. Pushes completed columns into the output ring
    /// (dropping oldest on overflow — matches the "gap, not freeze"
    /// policy).
    fn process(
        &mut self,
        device: &GpuDevice,
        left: &[f32],
        right: &[f32],
        cfg: &WorkerConfig,
        ring: &ArrayQueue<CqtColumnMsg>,
    ) {
        debug_assert_eq!(left.len(), right.len());
        let beats_per_hop = if cfg.bpm > 0.0 {
            self.hop as f64 / self.sample_rate as f64 * cfg.bpm as f64 / 60.0
        } else {
            0.0
        };

        let pending = self.samples_since_last_hop + left.len();
        let total_hops = pending / self.hop;
        let skip_hops = total_hops.saturating_sub(MAX_HOPS_PER_DRAIN);
        let mut hops_seen: usize = 0;
        let two_channels = matches!(cfg.source, SpectrogramSource::LeftRight);

        for (i, &l) in left.iter().enumerate() {
            let r = right[i];
            let (primary_s, secondary_s) = match cfg.source {
                SpectrogramSource::Mid => ((l + r) * 0.5, 0.0),
                SpectrogramSource::Side => ((l - r) * 0.5, 0.0),
                SpectrogramSource::LeftRight => (l, r),
            };
            self.primary.rolling[self.primary.rolling_head] = primary_s;
            self.primary.rolling_head = (self.primary.rolling_head + 1) % self.n_fft;
            if two_channels {
                self.secondary.rolling[self.secondary.rolling_head] = secondary_s;
                self.secondary.rolling_head =
                    (self.secondary.rolling_head + 1) % self.n_fft;
            }
            self.samples_since_last_hop += 1;
            if self.samples_since_last_hop >= self.hop {
                self.samples_since_last_hop = 0;
                hops_seen += 1;

                if hops_seen <= skip_hops {
                    // Skip expensive CQT but keep the beat clock consistent.
                    self.internal_beat_pos += beats_per_hop;
                    continue;
                }

                self.emit_column(device, cfg, ring, two_channels);
                self.internal_beat_pos += beats_per_hop;
            }
        }
    }

    fn run_channel_cqt(
        gpu_cqt: &mut GpuCqt,
        device: &GpuDevice,
        ch: &mut ChannelState,
        cfg: &WorkerConfig,
        n_fft: usize,
        hop: usize,
        sample_rate: f32,
        cqt_num_bins: usize,
        synchro_scratch: &mut [f32],
    ) {
        // Copy rolling ring into oldest→newest linear order.
        let head = ch.rolling_head;
        let (tail, front) = ch.rolling.split_at(head);
        ch.scratch_audio[..front.len()].copy_from_slice(front);
        ch.scratch_audio[front.len()..].copy_from_slice(tail);

        gpu_cqt.process(device, &ch.scratch_audio, &mut ch.cqt_complex);

        if cfg.synchrosqueeze && ch.have_prev_cqt {
            synchrosqueeze_into(
                gpu_cqt,
                &ch.cqt_complex,
                &ch.prev_cqt_complex,
                &ch.prev2_cqt_complex,
                ch.have_prev2_cqt,
                cfg,
                hop,
                sample_rate,
                cqt_num_bins,
                synchro_scratch,
                &mut ch.cqt_out,
            );
        } else {
            for (dst, c) in ch.cqt_out.iter_mut().zip(ch.cqt_complex.iter()) {
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
        if ch.have_prev_cqt {
            ch.prev2_cqt_complex.copy_from_slice(&ch.prev_cqt_complex);
            ch.have_prev2_cqt = true;
        }
        ch.prev_cqt_complex.copy_from_slice(&ch.cqt_complex);
        ch.have_prev_cqt = true;

        // Silence n_fft-only-warning when n_fft is unused in this branch
        // (kept in the signature so the caller's mental model is explicit).
        let _ = n_fft;
    }

    fn emit_column(
        &mut self,
        device: &GpuDevice,
        cfg: &WorkerConfig,
        ring: &ArrayQueue<CqtColumnMsg>,
        two_channels: bool,
    ) {
        Self::run_channel_cqt(
            &mut self.gpu_cqt,
            device,
            &mut self.primary,
            cfg,
            self.n_fft,
            self.hop,
            self.sample_rate,
            self.cqt_num_bins,
            &mut self.synchro_power_scratch,
        );
        if two_channels {
            Self::run_channel_cqt(
                &mut self.gpu_cqt,
                device,
                &mut self.secondary,
                cfg,
                self.n_fft,
                self.hop,
                self.sample_rate,
                self.cqt_num_bins,
                &mut self.synchro_power_scratch,
            );
        }

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

        let clear_first = std::mem::take(&mut self.clear_pending);
        let single_path = fill_count == 1 || !self.primary.have_prev_cqt_out;
        if single_path {
            // Hold current across the span (first hop after a mode change).
            let mut c = start_col;
            for i in 0..fill_count {
                let clear = clear_first && i == 0;
                let data2 = if two_channels {
                    Some(self.secondary.cqt_out.clone())
                } else {
                    None
                };
                force_push(
                    ring,
                    CqtColumnMsg {
                        col_idx: c,
                        data: self.primary.cqt_out.clone(),
                        data2,
                        clear_history_before: clear,
                    },
                );
                c = (c + 1) % self.history_cols;
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
                let data: Vec<f32> = lerp_power_db(
                    &self.primary.prev_cqt_out,
                    &self.primary.cqt_out,
                    one_minus_t,
                    t,
                );
                let data2 = if two_channels && self.secondary.have_prev_cqt_out {
                    Some(lerp_power_db(
                        &self.secondary.prev_cqt_out,
                        &self.secondary.cqt_out,
                        one_minus_t,
                        t,
                    ))
                } else if two_channels {
                    Some(self.secondary.cqt_out.clone())
                } else {
                    None
                };
                let clear = clear_first && i == 0;
                force_push(
                    ring,
                    CqtColumnMsg {
                        col_idx: c,
                        data,
                        data2,
                        clear_history_before: clear,
                    },
                );
                c = (c + 1) % self.history_cols;
            }
        }

        self.primary
            .prev_cqt_out
            .copy_from_slice(&self.primary.cqt_out);
        self.primary.have_prev_cqt_out = true;
        if two_channels {
            self.secondary
                .prev_cqt_out
                .copy_from_slice(&self.secondary.cqt_out);
            self.secondary.have_prev_cqt_out = true;
        }
    }
}

fn lerp_power_db(prev: &[f32], cur: &[f32], one_minus_t: f32, t: f32) -> Vec<f32> {
    prev.iter()
        .zip(cur.iter())
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
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn synchrosqueeze_into(
    gpu_cqt: &GpuCqt,
    cqt_complex: &[CqtComplex<f32>],
    prev_cqt_complex: &[CqtComplex<f32>],
    prev2_cqt_complex: &[CqtComplex<f32>],
    have_prev2_cqt: bool,
    cfg: &WorkerConfig,
    hop: usize,
    sample_rate: f32,
    num_bins: usize,
    scratch: &mut [f32],
    out_db: &mut [f32],
) {
    let center_freqs = gpu_cqt.center_freqs();
    let bandwidths = gpu_cqt.bandwidths_hz();
    let hop_f = hop as f32;
    let two_pi = std::f32::consts::TAU;
    let freq_per_rad_per_hop = sample_rate / (two_pi * hop_f);
    let hop_over_sr = hop_f / sample_rate;
    let mag_gate_power = 10.0_f32.powf(cfg.synchro_gate_db * 0.1);
    let unambiguous_hz = sample_rate / (2.0 * hop_f) * 0.8;
    let fmin = center_freqs[0];
    let log2_ratio = if num_bins > 1 {
        (center_freqs[1] / center_freqs[0]).log2()
    } else {
        1.0 / 24.0
    };
    let inv_log2_ratio = 1.0 / log2_ratio;

    scratch[..num_bins].fill(0.0);

    for k in 0..num_bins {
        let x_curr = cqt_complex[k];
        let x_prev = prev_cqt_complex[k];
        let x_prev2 = prev2_cqt_complex[k];
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

        if cfg.coherence && have_prev2_cqt && prev2_power >= mag_gate_power {
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
        scratch[lo] += power * (1.0 - frac);
        let hi = lo + 1;
        if hi < num_bins {
            scratch[hi] += power * frac;
        }
    }

    for (dst, &p) in out_db.iter_mut().zip(scratch[..num_bins].iter()) {
        *dst = if p > 1e-20 {
            10.0 * p.log10()
        } else {
            WORKER_FLOOR_DB
        };
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
    // Local scratch — keeping these off WorkerState means `process` can
    // take &mut self without borrowing gymnastics around a field mid-
    // drain. Two buffers because the audio thread pushes one stream per
    // channel into separate lock-free rings.
    //
    // Carry buffers absorb the boundary race: if the audio thread pushes
    // more samples between the worker's L-drain and R-drain calls, one
    // side will out-run the other by that block. Stashing the unpaired
    // tail keeps the L/R streams perfectly sample-aligned across loop
    // iterations — without it, every drain race would silently drop one
    // channel's tail and the L+R spectrogram would slowly skew apart.
    let mut left_scratch: Vec<f32> = Vec::with_capacity(8192);
    let mut right_scratch: Vec<f32> = Vec::with_capacity(8192);
    let mut left_carry: Vec<f32> = Vec::new();
    let mut right_carry: Vec<f32> = Vec::new();
    while !shutdown.load(Ordering::Acquire) {
        left_scratch.clear();
        right_scratch.clear();
        left_scratch.append(&mut left_carry);
        right_scratch.append(&mut right_carry);
        shared.left_sample_ring.drain_into(&mut left_scratch);
        shared.right_sample_ring.drain_into(&mut right_scratch);
        if left_scratch.is_empty() && right_scratch.is_empty() {
            thread::sleep(IDLE_SLEEP);
            continue;
        }
        let n = left_scratch.len().min(right_scratch.len());
        if n == 0 {
            // Only one side has data this iteration. Stash it back into
            // the carry so it pairs up with its counterpart next loop.
            left_carry.append(&mut left_scratch);
            right_carry.append(&mut right_scratch);
            thread::sleep(IDLE_SLEEP);
            continue;
        }
        let cfg = *config.lock();
        state.apply_config(cfg);
        state.process(
            &device,
            &left_scratch[..n],
            &right_scratch[..n],
            &cfg,
            &column_ring,
        );
        if left_scratch.len() > n {
            left_carry.extend_from_slice(&left_scratch[n..]);
        }
        if right_scratch.len() > n {
            right_carry.extend_from_slice(&right_scratch[n..]);
        }
    }
}
