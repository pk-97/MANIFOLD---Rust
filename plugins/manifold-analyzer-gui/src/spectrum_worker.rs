//! Dedicated stereo CQT worker thread.
//!
//! Audio thread pushes raw L + R samples into two `SampleRing`s; this
//! thread drains both in lockstep, runs two CQTs per hop, derives Mid /
//! Side / L / R / per-bin-correlation spectra + a (Mid) synchrosqueezed
//! column, and pushes completed columns into a bounded SPSC ring that
//! the GUI drains each redraw.
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

        // Build two GPU CQT pipelines — one for L, one for R. Each owns
        // its per-hop `fft_input` / `fft_output` / `cqt_output`
        // buffers, so running them back-to-back in `emit_column`
        // doesn't race the second write over the first's result.
        // Kernel buffer uploads + MPSGraph compile happen here.
        let gpu_cqt_l = GpuCqt::new(&device, &cqt);
        let gpu_cqt_r = GpuCqt::new(&device, &cqt);

        // Publish the CQT bin count + lazily size the shared mailboxes
        // before anyone reads from them.
        shared.resize_cqt_mailboxes(cqt_num_bins);

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
                    let mut state =
                        WorkerState::new(gpu_cqt_l, gpu_cqt_r, params, sample_rate);
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

    /// Two GPU pipelines — one for the L channel, one for R. Each is an
    /// independent R2C FFT + sparse CSR mat-vec plan; running them
    /// sequentially in one `emit_column` call produces two complex
    /// spectra, from which Mid / Side / L / R dB curves + per-bin
    /// correlation are derived on CPU.
    gpu_cqt_l: GpuCqt,
    gpu_cqt_r: GpuCqt,
    rolling_l: Vec<f32>,
    rolling_r: Vec<f32>,
    rolling_head: usize,
    samples_since_last_hop: usize,
    scratch_audio_l: Vec<f32>,
    scratch_audio_r: Vec<f32>,

    /// Complex CQT of the current hop's Mid channel. Synchrosqueeze
    /// still reads this (and the `prev*` copies) so its reassignment
    /// math is unchanged — we just feed it Mid derived from L/R
    /// instead of running a third CQT on the explicit Mid mix.
    cqt_complex: Vec<CqtComplex<f32>>,
    cqt_complex_l: Vec<CqtComplex<f32>>,
    cqt_complex_r: Vec<CqtComplex<f32>>,
    prev_cqt_complex: Vec<CqtComplex<f32>>,
    prev2_cqt_complex: Vec<CqtComplex<f32>>,
    have_prev_cqt: bool,
    have_prev2_cqt: bool,
    synchro_power_scratch: Vec<f32>,
    cqt_out: Vec<f32>,
    prev_cqt_out: Vec<f32>,
    have_prev_cqt_out: bool,

    // Asymmetric-EMA accumulators (power domain) for the SPAN-style
    // Mid/Side/L/R dB curves. Asymmetric so transients read as peaks
    // (fast attack, slow release), but attack is NOT instant — CQT's
    // high-freq bins have ~ms kernels, so instant attack would let
    // hop-to-hop noise teleport into the curve.
    power_avg_mid: Vec<f32>,
    power_avg_side: Vec<f32>,
    power_avg_l: Vec<f32>,
    power_avg_r: Vec<f32>,
    db_attack_alpha: f32,
    db_release_alpha: f32,

    // Symmetric-EMA accumulators (power domain) for per-bin
    // correlation. Steady 500 ms time constant so the colour strip
    // doesn't flicker.
    corr_power_l: Vec<f32>,
    corr_power_r: Vec<f32>,
    corr_re_lr: Vec<f32>,
    corr_alpha: f32,

    // Output buffers published to `shared.try_publish_*` each hop.
    mid_db_out: Vec<f32>,
    side_db_out: Vec<f32>,
    left_db_out: Vec<f32>,
    right_db_out: Vec<f32>,
    correlation_out: Vec<f32>,

    internal_beat_pos: f64,
    have_internal_beat: bool,
    write_col: u32,
    last_applied_cfg: WorkerConfig,
    /// Pending-clear flag — set when config change invalidates history.
    /// Consumed on the next emitted column so the GUI clears first, then
    /// writes the fresh data.
    clear_pending: bool,
}

/// 25 ms attack / 200 ms release. Attack is short but not instant so
/// CQT's near-zero time smoothing at high freq doesn't let hop-to-hop
/// variation teleport the curve. Release is long enough to read
/// transients as held peaks.
const DB_ATTACK_TC_S: f32 = 0.025;
const DB_RELEASE_TC_S: f32 = 0.200;
/// Symmetric 500 ms smoother for per-bin correlation — steady enough
/// that the colour strip doesn't flicker, fast enough that polarity
/// flips read within half a second.
const CORR_TC_S: f32 = 0.500;
/// Below this smoothed power (per channel) the correlation numerator
/// denominator drops to zero. Power floor mirrors the -120 dB floor
/// the old analyser used.
const CORR_POWER_FLOOR: f32 = 1.0e-12; // 10^(-120/10)

impl WorkerState {
    fn new(
        gpu_cqt_l: GpuCqt,
        gpu_cqt_r: GpuCqt,
        params: CqtBuildParams,
        sample_rate: f32,
    ) -> Self {
        let cqt_num_bins = gpu_cqt_l.num_bins();
        debug_assert_eq!(gpu_cqt_l.num_bins(), gpu_cqt_r.num_bins());
        let hop_seconds = params.hop_samples as f32 / sample_rate;
        // EMA alpha conversion. Both branches use `1 - exp(-dt/τ)`;
        // asymmetry comes from the different TCs. Correlation is
        // symmetric so we precompute one alpha.
        let db_attack_alpha = 1.0 - (-hop_seconds / DB_ATTACK_TC_S).exp();
        let db_release_alpha = 1.0 - (-hop_seconds / DB_RELEASE_TC_S).exp();
        let corr_alpha = 1.0 - (-hop_seconds / CORR_TC_S).exp();
        Self {
            n_fft: params.n_fft,
            hop: params.hop_samples,
            history_cols: params.history_cols,
            cqt_num_bins,
            sample_rate,
            gpu_cqt_l,
            gpu_cqt_r,
            rolling_l: vec![0.0; params.n_fft],
            rolling_r: vec![0.0; params.n_fft],
            rolling_head: 0,
            samples_since_last_hop: 0,
            scratch_audio_l: vec![0.0; params.n_fft],
            scratch_audio_r: vec![0.0; params.n_fft],
            cqt_complex: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            cqt_complex_l: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            cqt_complex_r: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            prev_cqt_complex: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            prev2_cqt_complex: vec![CqtComplex::new(0.0, 0.0); cqt_num_bins],
            have_prev_cqt: false,
            have_prev2_cqt: false,
            synchro_power_scratch: vec![0.0; cqt_num_bins],
            cqt_out: vec![WORKER_FLOOR_DB; cqt_num_bins],
            prev_cqt_out: vec![WORKER_FLOOR_DB; cqt_num_bins],
            have_prev_cqt_out: false,
            power_avg_mid: vec![0.0; cqt_num_bins],
            power_avg_side: vec![0.0; cqt_num_bins],
            power_avg_l: vec![0.0; cqt_num_bins],
            power_avg_r: vec![0.0; cqt_num_bins],
            db_attack_alpha,
            db_release_alpha,
            corr_power_l: vec![0.0; cqt_num_bins],
            corr_power_r: vec![0.0; cqt_num_bins],
            corr_re_lr: vec![0.0; cqt_num_bins],
            corr_alpha,
            mid_db_out: vec![WORKER_FLOOR_DB; cqt_num_bins],
            side_db_out: vec![WORKER_FLOOR_DB; cqt_num_bins],
            left_db_out: vec![WORKER_FLOOR_DB; cqt_num_bins],
            right_db_out: vec![WORKER_FLOOR_DB; cqt_num_bins],
            correlation_out: vec![0.0; cqt_num_bins],
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

    /// Process drained samples. `samples_l` / `samples_r` MUST be the same
    /// length — caller drains both rings and truncates to the shorter side
    /// so L and R stay sample-aligned. Runs CQT for the most recent hops,
    /// skipping older ones when the rings backed up. Pushes completed
    /// columns into the output ring (dropping oldest on overflow — matches
    /// the "gap, not freeze" policy).
    fn process(
        &mut self,
        device: &GpuDevice,
        shared: &AnalyzerGuiShared,
        samples_l: &[f32],
        samples_r: &[f32],
        cfg: &WorkerConfig,
        ring: &ArrayQueue<CqtColumnMsg>,
    ) {
        debug_assert_eq!(samples_l.len(), samples_r.len());
        let beats_per_hop = if cfg.bpm > 0.0 {
            self.hop as f64 / self.sample_rate as f64 * cfg.bpm as f64 / 60.0
        } else {
            0.0
        };

        let pending = self.samples_since_last_hop + samples_l.len();
        let total_hops = pending / self.hop;
        let skip_hops = total_hops.saturating_sub(MAX_HOPS_PER_DRAIN);
        let mut hops_seen: usize = 0;

        for i in 0..samples_l.len() {
            self.rolling_l[self.rolling_head] = samples_l[i];
            self.rolling_r[self.rolling_head] = samples_r[i];
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

                self.emit_column(device, shared, cfg, ring);
                self.internal_beat_pos += beats_per_hop;
            }
        }
    }

    fn emit_column(
        &mut self,
        device: &GpuDevice,
        shared: &AnalyzerGuiShared,
        cfg: &WorkerConfig,
        ring: &ArrayQueue<CqtColumnMsg>,
    ) {
        // Copy both rolling rings into oldest→newest linear order.
        let head = self.rolling_head;
        {
            let (tail, front) = self.rolling_l.split_at(head);
            self.scratch_audio_l[..front.len()].copy_from_slice(front);
            self.scratch_audio_l[front.len()..].copy_from_slice(tail);
        }
        {
            let (tail, front) = self.rolling_r.split_at(head);
            self.scratch_audio_r[..front.len()].copy_from_slice(front);
            self.scratch_audio_r[front.len()..].copy_from_slice(tail);
        }

        // Run L + R CQT in sequence. Each instance owns its own
        // per-hop buffers so the second doesn't stomp the first's
        // output.
        self.gpu_cqt_l
            .process(device, &self.scratch_audio_l, &mut self.cqt_complex_l);
        self.gpu_cqt_r
            .process(device, &self.scratch_audio_r, &mut self.cqt_complex_r);

        // Derive Mid spectrum for the spectrogram + synchrosqueeze
        // pipeline, and per-bin power / cross-power for the curves
        // and correlation. One pass over `cqt_num_bins`, all in
        // pre-allocated scratch.
        let attack_alpha = self.db_attack_alpha;
        let release_alpha = self.db_release_alpha;
        let corr_alpha = self.corr_alpha;
        let one_minus_corr_alpha = 1.0 - corr_alpha;
        for k in 0..self.cqt_num_bins {
            let l = self.cqt_complex_l[k];
            let r = self.cqt_complex_r[k];
            // Mid / Side derivation. `0.5 * (L ± R)` keeps the unitless
            // scaling consistent with an explicit (L+R)/2 and (L-R)/2
            // Mid/Side mix fed through the same CQT.
            let m_re = 0.5 * (l.re + r.re);
            let m_im = 0.5 * (l.im + r.im);
            let s_re = 0.5 * (l.re - r.re);
            let s_im = 0.5 * (l.im - r.im);
            self.cqt_complex[k] = CqtComplex::new(m_re, m_im);

            let p_l = l.re * l.re + l.im * l.im;
            let p_r = r.re * r.re + r.im * r.im;
            let p_m = m_re * m_re + m_im * m_im;
            let p_s = s_re * s_re + s_im * s_im;
            let re_lr = l.re * r.re + l.im * r.im;

            // Asymmetric EMA on each dB curve.
            // Attack 25 ms, release 200 ms. Short attack keeps transients
            // visible without letting a single noisy hop teleport the
            // curve; long release reads peaks as held.
            macro_rules! asym_ema {
                ($acc:expr, $p:expr) => {{
                    let prev = $acc[k];
                    let alpha = if $p >= prev { attack_alpha } else { release_alpha };
                    let new_avg = alpha * $p + (1.0 - alpha) * prev;
                    $acc[k] = new_avg;
                    new_avg
                }};
            }
            let avg_m = asym_ema!(self.power_avg_mid, p_m);
            let avg_s = asym_ema!(self.power_avg_side, p_s);
            let avg_l = asym_ema!(self.power_avg_l, p_l);
            let avg_r = asym_ema!(self.power_avg_r, p_r);
            self.mid_db_out[k] = 10.0 * (avg_m + 1.0e-24).log10();
            self.side_db_out[k] = 10.0 * (avg_s + 1.0e-24).log10();
            self.left_db_out[k] = 10.0 * (avg_l + 1.0e-24).log10();
            self.right_db_out[k] = 10.0 * (avg_r + 1.0e-24).log10();

            // Symmetric EMA on power / cross-power for correlation.
            let cp_l =
                corr_alpha * p_l + one_minus_corr_alpha * self.corr_power_l[k];
            let cp_r =
                corr_alpha * p_r + one_minus_corr_alpha * self.corr_power_r[k];
            let cr_lr =
                corr_alpha * re_lr + one_minus_corr_alpha * self.corr_re_lr[k];
            self.corr_power_l[k] = cp_l;
            self.corr_power_r[k] = cp_r;
            self.corr_re_lr[k] = cr_lr;
            self.correlation_out[k] = if cp_l < CORR_POWER_FLOOR || cp_r < CORR_POWER_FLOOR {
                0.0
            } else {
                let denom = (cp_l * cp_r).sqrt().max(1.0e-20);
                (cr_lr / denom).clamp(-1.0, 1.0)
            };
        }

        // Publish the latest-frame curves + correlation to the shared
        // mailboxes. Try-lock so the GUI reader can never block us.
        let _ = shared.try_publish_mid_db(&self.mid_db_out);
        let _ = shared.try_publish_side_db(&self.side_db_out);
        let _ = shared.try_publish_left_db(&self.left_db_out);
        let _ = shared.try_publish_right_db(&self.right_db_out);
        let _ = shared.try_publish_correlation(&self.correlation_out);

        // Spectrogram column: synchrosqueeze the Mid complex spectrum
        // (same behaviour as the pre-Phase-B single-CQT path, but now
        // `cqt_complex` is derived Mid rather than a separately-CQT'd
        // mono Mid mix).
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
        // Synchrosqueeze is Mid-only; center_freqs/bandwidths come
        // from either CQT (L and R use identical layouts).
        let center_freqs = self.gpu_cqt_l.center_freqs();
        let bandwidths = self.gpu_cqt_l.bandwidths_hz();
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
    // Local scratches — keeping these off WorkerState means `process`
    // can take `&mut self` without borrowing gymnastics around a field
    // mid-drain.
    let mut left_scratch: Vec<f32> = Vec::with_capacity(8192);
    let mut right_scratch: Vec<f32> = Vec::with_capacity(8192);
    while !shutdown.load(Ordering::Acquire) {
        left_scratch.clear();
        right_scratch.clear();
        let drained_l = shared.left_sample_ring.drain_into(&mut left_scratch);
        let drained_r = shared.right_sample_ring.drain_into(&mut right_scratch);
        // Keep L and R sample-aligned. The audio thread pushes into
        // both rings in lockstep, so in steady state they drain to the
        // same length — but after a stall one side may have dropped
        // samples ahead of the other. Truncating to the shorter side
        // keeps the next hop on phase; the unpaired remainder is
        // dropped (same "gap, not stutter" philosophy as the mono
        // path).
        if drained_l == 0 && drained_r == 0 {
            thread::sleep(IDLE_SLEEP);
            continue;
        }
        let paired = drained_l.min(drained_r);
        if paired == 0 {
            continue;
        }
        left_scratch.truncate(paired);
        right_scratch.truncate(paired);
        let cfg = *config.lock();
        state.apply_config(cfg);
        state.process(
            &device,
            &shared,
            &left_scratch,
            &right_scratch,
            &cfg,
            &column_ring,
        );
    }
}
