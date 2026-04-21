//! ITU-R BS.1770-4 / EBU R128 loudness meter.
//!
//! Produces Momentary (400 ms), Short-term (3 s), Integrated (gated),
//! Loudness Range, True Peak, and the peak-to-loudness / dynamic-range
//! figures for a stereo stream. All math is on the audio thread;
//! results are plain scalars the GUI reads via atomics.
//!
//! Time-base: every new input sample is K-weighted (pre-shelf + RLB
//! high-pass), squared, and accumulated into a running 100 ms sum.
//! When 100 ms elapses, the per-channel mean-square is closed out and
//! pushed into ring buffers that drive both the sliding M/S windows
//! and the gated integrated / LRA calculations.
//!
//! True peak uses 4× polyphase oversampling with a windowed-sinc FIR.
//! We also fold in the raw sample peak so the reported TP is always
//! ≥ the actual sample-rate max (defensive against FIR roll-off).
//!
//! Not a certification meter — this is tuned for live display: cheap
//! enough to run on the plugin's audio thread and fast enough to
//! visually react within a block. Numerical behaviour is within a
//! fraction of a dB of reference implementations on sine sweeps.

use crossbeam_queue::ArrayQueue;
use std::sync::Arc;

const ABSOLUTE_GATE_LUFS: f32 = -70.0;
const RELATIVE_GATE_LU: f32 = 10.0;
const LRA_RELATIVE_GATE_LU: f32 = 20.0;
const LKFS_OFFSET: f32 = -0.691;
const MIN_LUFS: f32 = -120.0;

/// Amplitude → dB with a floor matching `MIN_LUFS` so silent
/// passages report the same "no signal" sentinel as the LUFS track.
fn amp_to_db(a: f32) -> f32 {
    if a <= 1e-12 { MIN_LUFS } else { 20.0 * a.log10() }
}

const BLOCK_MS: f32 = 100.0;
const MOMENTARY_BLOCKS: usize = 4; // 400 ms / 100 ms
const SHORT_TERM_BLOCKS: usize = 30; // 3000 ms / 100 ms
const LRA_BLOCKS_PER_UPDATE: usize = SHORT_TERM_BLOCKS;

const TP_OVERSAMPLE: usize = 4;
// 16 taps/phase × 0.985 cutoff brings the FIR's DC droop to < 0.02 dB
// (was ~0.2 dB at 12 taps / 0.97 cutoff). Raw-sample-peak fold-in still
// guards against any residual under-reading at integer offsets.
const TP_TAPS_PER_PHASE: usize = 16;

/// Direct-form-II biquad. State kept in `z1`, `z2`.
#[derive(Default, Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// Pre-filter stage of K-weighting. High-shelf at ~1.68 kHz, +4 dB.
/// Coefficients derived via bilinear transform from the analog
/// prototype (identical formulation to libebur128).
fn k_pre_filter(sample_rate: f32) -> Biquad {
    // BS.1770 reference parameters. Computed in f64 so the derived
    // biquad coefficients match the canonical 48 kHz values to f32
    // precision at any sample rate.
    const F0: f64 = 1_681.974_450_955_533;
    const G: f64 = 3.999_843_853_973_347;
    const Q: f64 = 0.707_175_236_955_419_6;
    const VB_EXP: f64 = 0.499_666_774_154_541_6;
    let k = (std::f64::consts::PI * F0 / sample_rate as f64).tan();
    let k2 = k * k;
    let vh = 10.0_f64.powf(G / 20.0);
    let vb = vh.powf(VB_EXP);
    let a0 = 1.0 + k / Q + k2;
    Biquad {
        b0: ((vh + vb * k / Q + k2) / a0) as f32,
        b1: (2.0 * (k2 - vh) / a0) as f32,
        b2: ((vh - vb * k / Q + k2) / a0) as f32,
        a1: (2.0 * (k2 - 1.0) / a0) as f32,
        a2: ((1.0 - k / Q + k2) / a0) as f32,
        z1: 0.0,
        z2: 0.0,
    }
}

/// RLB stage: 2nd-order Butterworth-ish high-pass at ~38 Hz, removes
/// sub-bass before summing.
///
/// Normalisation matches ITU-R BS.1770-4 Annex 1: the numerator stays
/// at the HPF prototype `(1, -2, 1)` un-normalised, and only the
/// denominator is divided by a0. This is what the WGSL display shader
/// hardcodes, so meter and LUFS-weighting display agree to f32
/// precision (instead of differing by ~0.04 dB due to normalisation
/// convention).
fn k_rlb_filter(sample_rate: f32) -> Biquad {
    const F0: f64 = 38.135_470_876_024_44;
    const Q: f64 = 0.500_327_037_323_877_3;
    let k = (std::f64::consts::PI * F0 / sample_rate as f64).tan();
    let k2 = k * k;
    let a0 = 1.0 + k / Q + k2;
    Biquad {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: (2.0 * (k2 - 1.0) / a0) as f32,
        a2: ((1.0 - k / Q + k2) / a0) as f32,
        z1: 0.0,
        z2: 0.0,
    }
}

#[derive(Default)]
struct ChannelKFilter {
    pre: Biquad,
    rlb: Biquad,
}

impl ChannelKFilter {
    fn new(sample_rate: f32) -> Self {
        Self {
            pre: k_pre_filter(sample_rate),
            rlb: k_rlb_filter(sample_rate),
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        self.rlb.process(self.pre.process(x))
    }

    fn reset(&mut self) {
        self.pre.reset();
        self.rlb.reset();
    }
}

/// 4× oversampled true-peak detector for a stereo pair. Polyphase
/// windowed-sinc FIR; we also fold in the raw sample peak so the
/// reported maximum is never *below* sample-rate peak.
struct TruePeakDetector {
    phases: Vec<Vec<f32>>,
    hist_l: Vec<f32>,
    hist_r: Vec<f32>,
    write: usize,
    peak_abs: f32,
}

impl TruePeakDetector {
    fn new() -> Self {
        let total = TP_OVERSAMPLE * TP_TAPS_PER_PHASE;
        let mut h = vec![0.0_f32; total];
        let center = (total as f32 - 1.0) * 0.5;
        // Cutoff slightly below 1/L so zero-crossings nearly line up
        // with integer-sample offsets, preserving unity gain at DC
        // while passing essentially the full original band.
        let cutoff = 0.985;
        for (n, coef) in h.iter_mut().enumerate().take(total) {
            let k = n as f32 - center;
            let arg = std::f32::consts::PI * cutoff * k / TP_OVERSAMPLE as f32;
            let sinc = if arg.abs() < 1e-8 {
                1.0
            } else {
                arg.sin() / arg
            };
            let w = 0.5
                - 0.5
                    * (2.0 * std::f32::consts::PI * n as f32 / (total as f32 - 1.0)).cos();
            *coef = sinc * w;
        }
        // Normalise so each phase has unity gain at DC (sum → 1).
        let sum: f32 = h.iter().sum();
        if sum > 0.0 {
            let scale = TP_OVERSAMPLE as f32 / sum;
            for v in &mut h {
                *v *= scale;
            }
        }
        let mut phases: Vec<Vec<f32>> = (0..TP_OVERSAMPLE)
            .map(|_| Vec::with_capacity(TP_TAPS_PER_PHASE))
            .collect();
        for (n, &coef) in h.iter().enumerate() {
            phases[n % TP_OVERSAMPLE].push(coef);
        }
        Self {
            phases,
            hist_l: vec![0.0; TP_TAPS_PER_PHASE],
            hist_r: vec![0.0; TP_TAPS_PER_PHASE],
            write: 0,
            peak_abs: 0.0,
        }
    }

    fn process_pair(&mut self, xl: f32, xr: f32) {
        self.hist_l[self.write] = xl;
        self.hist_r[self.write] = xr;
        // Fold in the raw sample peak — guards against FIR loss at
        // integer positions (our cutoff is 0.97, not exactly 1.0).
        let raw_peak = xl.abs().max(xr.abs());
        if raw_peak > self.peak_abs {
            self.peak_abs = raw_peak;
        }
        let taps = TP_TAPS_PER_PHASE;
        for phase in &self.phases {
            let mut acc_l = 0.0_f32;
            let mut acc_r = 0.0_f32;
            for (j, &h) in phase.iter().enumerate() {
                let idx = (self.write + taps - j) % taps;
                acc_l += h * self.hist_l[idx];
                acc_r += h * self.hist_r[idx];
            }
            let mag = acc_l.abs().max(acc_r.abs());
            if mag > self.peak_abs {
                self.peak_abs = mag;
            }
        }
        self.write = (self.write + 1) % taps;
    }

    fn peak_dbtp(&self) -> f32 {
        if self.peak_abs <= 1e-12 {
            -120.0
        } else {
            20.0 * self.peak_abs.log10()
        }
    }

    fn reset(&mut self) {
        for v in &mut self.hist_l {
            *v = 0.0;
        }
        for v in &mut self.hist_r {
            *v = 0.0;
        }
        self.write = 0;
        self.peak_abs = 0.0;
    }
}

/// Snapshot of all loudness readouts, published after each process
/// block. All values are in LUFS / LU / dBTP; `MIN_LUFS` means "not
/// yet computed".
#[derive(Debug, Clone, Copy)]
pub struct LoudnessSnapshot {
    pub momentary_lufs: f32,
    pub short_term_lufs: f32,
    pub integrated_lufs: f32,
    pub lra_lu: f32,
    pub dr_lu: f32,
    pub plr_lu: f32,
    pub momentary_max_lufs: f32,
    pub short_term_max_lufs: f32,
    pub true_peak_max_dbtp: f32,
    /// Live sample-peak (dBFS, 300 ms release hold) across L/R.
    pub sample_peak_db: f32,
    /// Monotonic max of the raw sample peak (dBFS).
    pub sample_peak_max_db: f32,
    /// Smoothed stereo RMS (dBFS, 300 ms time constant).
    pub rms_db: f32,
    /// Monotonic max of the smoothed RMS (dBFS).
    pub rms_max_db: f32,
    /// Smoothed Pearson correlation between L and R: +1 mono,
    /// 0 uncorrelated, −1 anti-phase.
    pub correlation: f32,
    /// Seconds of audio processed since the last `reset()`. Lets the
    /// GUI suppress metrics that need a warm-up window (ST, Integrated,
    /// LRA, DR, PLR, ST Max) behind a sensible threshold instead of
    /// showing noisy partial readings.
    pub elapsed_secs: f32,
}

impl LoudnessSnapshot {
    pub const EMPTY: Self = Self {
        momentary_lufs: MIN_LUFS,
        short_term_lufs: MIN_LUFS,
        integrated_lufs: MIN_LUFS,
        lra_lu: 0.0,
        dr_lu: 0.0,
        plr_lu: 0.0,
        momentary_max_lufs: MIN_LUFS,
        short_term_max_lufs: MIN_LUFS,
        true_peak_max_dbtp: MIN_LUFS,
        sample_peak_db: MIN_LUFS,
        sample_peak_max_db: MIN_LUFS,
        rms_db: MIN_LUFS,
        rms_max_db: MIN_LUFS,
        correlation: 0.0,
        elapsed_secs: 0.0,
    };
}

pub struct LoudnessMeter {
    sample_rate: f32,
    samples_per_block: usize,

    k_l: ChannelKFilter,
    k_r: ChannelKFilter,
    tp: TruePeakDetector,

    // Per-sample live readouts for Peak / RMS / Correlation. All
    // 300 ms-time-constant — fast enough for mastering, slow enough
    // that the numbers don't chatter.
    peak_hold: f32,        // max(|L|, |R|) with exponential release
    peak_release_coef: f32,
    sm_l_mean_sq: f32,     // 1-pole smoothed L²
    sm_r_mean_sq: f32,     // 1-pole smoothed R²
    sm_lr_mean: f32,       // 1-pole smoothed L·R (correlation numerator)
    sm_coef: f32,          // 1 − (1-pole decay factor per sample)
    raw_peak_abs_max: f32, // monotonic max of |L|, |R| raw samples
    rms_max_linear: f32,   // monotonic max of sqrt(sm_mean_sq) for display

    // 100 ms accumulator, per channel, sum of squares.
    block_sum_sq_l: f64,
    block_sum_sq_r: f64,
    block_count: usize,

    // Closed 100 ms bins, channel-weighted mean-square (L² + R²) / N
    // where N is samples_per_block. Stored indefinitely so gated
    // integrated / LRA can be recomputed at each 100 ms tick. Only
    // written when no `block_sink` is attached — when a sink is in
    // place, the worker thread owns the bin history.
    block_msq: Vec<f32>,

    // Per-tick scratch buffers — promoted to fields so the audio-thread
    // recompute path stays allocation-free across a long session
    // (otherwise each field would be re-allocated every 100 ms, up to
    // ~144 KB each at 1 hr). Unused when a `block_sink` is attached.
    scratch_block_means: Vec<f32>,
    scratch_lra_loudness: Vec<f32>,
    scratch_kept: Vec<f32>,

    // Optional sink for closed-block z values. When present (plugin
    // runtime path), the meter stops running integrated / LRA gating
    // in-line; a worker thread pops z's from this queue and does the
    // O(N) recompute off-thread. Absent in CLI / unit tests so the
    // standalone `snapshot()` keeps working.
    block_sink: Option<Arc<ArrayQueue<f32>>>,

    // Most recent readouts.
    snapshot: LoudnessSnapshot,

    // Counter of closed 100 ms bins, used to schedule recomputation
    // of integrated/LRA every 100 ms.
    bins_since_reset: usize,
}

impl LoudnessMeter {
    pub fn new(sample_rate: f32) -> Self {
        let samples_per_block = ((sample_rate * BLOCK_MS / 1000.0).round() as usize).max(1);
        // Pre-size block storage + scratch for ~30 min sessions so the
        // audio-thread push never allocates in the common case. Beyond
        // that, Vec amortised growth kicks in (still bounded memcpy).
        const PRESIZE_BINS: usize = 18_000; // 30 min × 10 bins/sec
        // 300 ms time constant for peak release and RMS/correlation
        // smoothing. `coef^N = e^-1` at N = sr·0.3 samples.
        const READOUT_TC_S: f32 = 0.3;
        let sm_coef = (-1.0 / (sample_rate * READOUT_TC_S).max(1.0)).exp();
        Self {
            sample_rate,
            samples_per_block,
            k_l: ChannelKFilter::new(sample_rate),
            k_r: ChannelKFilter::new(sample_rate),
            tp: TruePeakDetector::new(),
            peak_hold: 0.0,
            peak_release_coef: sm_coef,
            sm_l_mean_sq: 0.0,
            sm_r_mean_sq: 0.0,
            sm_lr_mean: 0.0,
            sm_coef,
            raw_peak_abs_max: 0.0,
            rms_max_linear: 0.0,
            block_sum_sq_l: 0.0,
            block_sum_sq_r: 0.0,
            block_count: 0,
            block_msq: Vec::with_capacity(PRESIZE_BINS),
            scratch_block_means: Vec::with_capacity(PRESIZE_BINS),
            scratch_lra_loudness: Vec::with_capacity(PRESIZE_BINS),
            scratch_kept: Vec::with_capacity(PRESIZE_BINS),
            block_sink: None,
            snapshot: LoudnessSnapshot::EMPTY,
            bins_since_reset: 0,
        }
    }

    /// Set the current integrated-LUFS value from an external source
    /// (typically the off-thread worker that computes gating). Lets the
    /// audio-thread DR / PLR derivation inside `update_running_readouts`
    /// use an up-to-date integrated instead of the short-term-max
    /// fallback it would otherwise be stuck on when a sink is attached.
    pub fn set_external_integrated_lufs(&mut self, lufs: f32) {
        self.snapshot.integrated_lufs = lufs;
    }

    /// Attach an off-thread sink that receives closed-block z values.
    /// With a sink attached, the meter stops computing integrated / LRA
    /// in-line and no longer stores historical `block_msq` — the worker
    /// on the other end of the queue owns that work.
    ///
    /// Overflow uses `force_push`: if the worker falls behind by more than
    /// the queue capacity, the oldest pending block is dropped. For a
    /// reasonable queue size (256 = 25.6 s) this never happens in
    /// practice, but it guarantees the audio thread never blocks.
    pub fn attach_block_sink(&mut self, sink: Arc<ArrayQueue<f32>>) {
        self.block_sink = Some(sink);
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn snapshot(&self) -> LoudnessSnapshot {
        self.snapshot
    }

    pub fn reset(&mut self) {
        self.k_l.reset();
        self.k_r.reset();
        self.tp.reset();
        self.peak_hold = 0.0;
        self.sm_l_mean_sq = 0.0;
        self.sm_r_mean_sq = 0.0;
        self.sm_lr_mean = 0.0;
        self.raw_peak_abs_max = 0.0;
        self.rms_max_linear = 0.0;
        self.block_sum_sq_l = 0.0;
        self.block_sum_sq_r = 0.0;
        self.block_count = 0;
        self.block_msq.clear();
        self.scratch_block_means.clear();
        self.scratch_lra_loudness.clear();
        self.scratch_kept.clear();
        self.snapshot = LoudnessSnapshot::EMPTY;
        self.bins_since_reset = 0;
    }

    /// Push a block of stereo samples. Samples are expected
    /// interleaved as separate slices (one per channel). Mono
    /// callers can pass the same slice twice.
    pub fn process(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len().min(right.len());
        let sm_coef = self.sm_coef;
        let one_minus = 1.0 - sm_coef;
        let release = self.peak_release_coef;
        for i in 0..n {
            let xl = left[i];
            let xr = right[i];
            self.tp.process_pair(xl, xr);
            let kl = self.k_l.process(xl);
            let kr = self.k_r.process(xr);
            self.block_sum_sq_l += (kl * kl) as f64;
            self.block_sum_sq_r += (kr * kr) as f64;
            self.block_count += 1;
            if self.block_count >= self.samples_per_block {
                self.close_block();
            }

            // Sample-peak hold: instant attack, exponential release.
            // Tracks raw (un-weighted) sample magnitude so it pairs
            // with crest-factor / PPM conventions.
            let abs_peak = xl.abs().max(xr.abs());
            if abs_peak > self.peak_hold {
                self.peak_hold = abs_peak;
            } else {
                self.peak_hold *= release;
            }
            if abs_peak > self.raw_peak_abs_max {
                self.raw_peak_abs_max = abs_peak;
            }

            // 1-pole smoothed L², R², L·R for RMS + correlation.
            // Using `sm_coef * prev + (1-sm_coef) * x` yields a 300 ms
            // time-constant LPF on the instantaneous power / product.
            self.sm_l_mean_sq = sm_coef * self.sm_l_mean_sq + one_minus * (xl * xl);
            self.sm_r_mean_sq = sm_coef * self.sm_r_mean_sq + one_minus * (xr * xr);
            self.sm_lr_mean = sm_coef * self.sm_lr_mean + one_minus * (xl * xr);
        }
        // Refresh running readouts (M, ST, max, TP) even if no bin
        // closed this call — M/ST don't change within a 100 ms bin,
        // but TP does.
        self.update_running_readouts();
    }

    fn close_block(&mut self) {
        let n = self.block_count.max(1) as f64;
        let msq_l = self.block_sum_sq_l / n;
        let msq_r = self.block_sum_sq_r / n;
        // Stereo channel-weighted sum (G_L = G_R = 1 per BS.1770-4
        // for front L/R). Future 5.1 support would apply G_Ls = 1.41.
        let z = (msq_l + msq_r) as f32;
        self.block_sum_sq_l = 0.0;
        self.block_sum_sq_r = 0.0;
        self.block_count = 0;
        self.bins_since_reset += 1;

        if let Some(sink) = &self.block_sink {
            // Deferred path: hand z to the worker thread. force_push
            // evicts the oldest on overflow so we never block the audio
            // thread. Integrated / LRA will land on the atomics a few
            // hops later; consumers already treat those readouts as
            // slow-moving.
            let _ = sink.force_push(z);
            // Also stash z in a small rolling window so the in-line
            // momentary / short-term readouts have real history — the
            // worker owns the unbounded log for integrated / LRA, not
            // these fast-path readouts. Without this the partial is
            // the ONLY contributor and ST flickers around a 100 ms
            // window.
            self.block_msq.push(z);
            const ROLLING_CAP: usize = SHORT_TERM_BLOCKS + 2;
            if self.block_msq.len() > ROLLING_CAP {
                let drop = self.block_msq.len() - ROLLING_CAP;
                self.block_msq.drain(..drop);
            }
        } else {
            // In-line path (CLI, unit tests): keep full history +
            // recompute integrated / LRA in-line on every tick.
            self.block_msq.push(z);
            self.recompute_on_tick();
        }
    }

    fn update_running_readouts(&mut self) {
        // Momentary/short-term include the in-progress 100 ms bin as
        // a partial contribution (time-weighted by its sample count).
        // Without this the readouts step at 10 Hz, which looks like
        // stuttering on a 60 fps meter — including the partial makes
        // the column update every audio block instead.
        let m_opt = self.windowed_mean_sq_with_partial(MOMENTARY_BLOCKS);
        let s_opt = self.windowed_mean_sq_with_partial(SHORT_TERM_BLOCKS);
        if let Some(m_mean) = m_opt {
            let m = loudness_from_mean_sq(m_mean);
            self.snapshot.momentary_lufs = m;
            if m > self.snapshot.momentary_max_lufs {
                self.snapshot.momentary_max_lufs = m;
            }
        } else {
            self.snapshot.momentary_lufs = MIN_LUFS;
        }
        if let Some(s_mean) = s_opt {
            let s = loudness_from_mean_sq(s_mean);
            self.snapshot.short_term_lufs = s;
            if s > self.snapshot.short_term_max_lufs {
                self.snapshot.short_term_max_lufs = s;
            }
        } else {
            self.snapshot.short_term_lufs = MIN_LUFS;
        }
        let tp = self.tp.peak_dbtp();
        if tp > self.snapshot.true_peak_max_dbtp {
            self.snapshot.true_peak_max_dbtp = tp;
        }
        // DR = ST max - Integrated (falls back to ST if I unknown).
        // PLR = TP max - Integrated (or TP max - ST max as fallback).
        let integrated = self.snapshot.integrated_lufs;
        let ref_lufs = if integrated > MIN_LUFS {
            integrated
        } else {
            self.snapshot.short_term_max_lufs
        };
        if ref_lufs > MIN_LUFS && self.snapshot.short_term_max_lufs > MIN_LUFS {
            self.snapshot.dr_lu = self.snapshot.short_term_max_lufs - ref_lufs;
        }
        if ref_lufs > MIN_LUFS && self.snapshot.true_peak_max_dbtp > MIN_LUFS {
            self.snapshot.plr_lu = self.snapshot.true_peak_max_dbtp - ref_lufs;
        }

        // Sample-peak live + max.
        let peak_live_db = amp_to_db(self.peak_hold);
        let peak_raw_db = amp_to_db(self.raw_peak_abs_max);
        self.snapshot.sample_peak_db = peak_live_db;
        if peak_raw_db > self.snapshot.sample_peak_max_db {
            self.snapshot.sample_peak_max_db = peak_raw_db;
        }

        // Stereo RMS: power-average the two smoothed mean-squares.
        let rms_ms = 0.5 * (self.sm_l_mean_sq + self.sm_r_mean_sq);
        let rms_linear = rms_ms.max(0.0).sqrt();
        if rms_linear > self.rms_max_linear {
            self.rms_max_linear = rms_linear;
        }
        self.snapshot.rms_db = amp_to_db(rms_linear);
        self.snapshot.rms_max_db = amp_to_db(self.rms_max_linear);

        // Elapsed time since last reset. Closed-block count gives us
        // 100 ms granularity; fold in the in-progress partial bin for
        // smooth sub-block progress on long pieces.
        self.snapshot.elapsed_secs = self.bins_since_reset as f32 * (BLOCK_MS * 0.001)
            + self.block_count as f32 / self.sample_rate.max(1.0);

        // Pearson correlation between L and R. Denominator is the
        // geometric mean of smoothed L² and R²; guard with a small
        // epsilon so silent passages show 0 instead of NaN. Clamp to
        // [-1, 1] in case round-off pushes the ratio slightly past.
        let denom_sq = self.sm_l_mean_sq * self.sm_r_mean_sq;
        self.snapshot.correlation = if denom_sq > 1e-18 {
            (self.sm_lr_mean / denom_sq.sqrt()).clamp(-1.0, 1.0)
        } else {
            0.0
        };
    }

    fn recompute_on_tick(&mut self) {
        // Integrated loudness: two-pass gate over all 400 ms blocks
        // (100 ms stride). A 400 ms block is the mean of 4
        // consecutive 100 ms bins; we iterate by starting bin.
        let n_bins = self.block_msq.len();
        if n_bins < MOMENTARY_BLOCKS {
            return;
        }
        let stride = 1; // 75 % overlap = one bin step
        let window = MOMENTARY_BLOCKS;
        let mut ungated_sum = 0.0_f64;
        let mut ungated_count = 0_usize;
        let end = n_bins - window + 1;
        self.scratch_block_means.clear();
        for start in (0..end).step_by(stride) {
            let m = mean_slice(&self.block_msq[start..start + window]);
            self.scratch_block_means.push(m);
            if loudness_from_mean_sq(m) >= ABSOLUTE_GATE_LUFS {
                ungated_sum += m as f64;
                ungated_count += 1;
            }
        }
        if ungated_count == 0 {
            return;
        }
        let ungated_mean = (ungated_sum / ungated_count as f64) as f32;
        let rel_threshold = loudness_from_mean_sq(ungated_mean) - RELATIVE_GATE_LU;
        let mut gated_sum = 0.0_f64;
        let mut gated_count = 0_usize;
        for &m in &self.scratch_block_means {
            let lufs = loudness_from_mean_sq(m);
            if lufs >= ABSOLUTE_GATE_LUFS && lufs >= rel_threshold {
                gated_sum += m as f64;
                gated_count += 1;
            }
        }
        if gated_count > 0 {
            let gated_mean = (gated_sum / gated_count as f64) as f32;
            self.snapshot.integrated_lufs = loudness_from_mean_sq(gated_mean);
        }

        // LRA: same gating but on 3 s short-term blocks (30 bins).
        // Gate: absolute −70 LUFS + relative −20 LU. Range = 95th −
        // 10th percentile of the surviving loudness values.
        if n_bins >= LRA_BLOCKS_PER_UPDATE {
            let lra_window = LRA_BLOCKS_PER_UPDATE;
            let lra_end = n_bins - lra_window + 1;
            self.scratch_lra_loudness.clear();
            let mut lra_ungated_sum = 0.0_f64;
            let mut lra_ungated_count = 0_usize;
            for start in (0..lra_end).step_by(stride) {
                let m = mean_slice(&self.block_msq[start..start + lra_window]);
                let lufs = loudness_from_mean_sq(m);
                if lufs >= ABSOLUTE_GATE_LUFS {
                    lra_ungated_sum += m as f64;
                    lra_ungated_count += 1;
                    self.scratch_lra_loudness.push(lufs);
                }
            }
            if lra_ungated_count > 0 {
                let lra_ungated_mean = (lra_ungated_sum / lra_ungated_count as f64) as f32;
                let lra_rel = loudness_from_mean_sq(lra_ungated_mean) - LRA_RELATIVE_GATE_LU;
                self.scratch_kept.clear();
                self.scratch_kept
                    .extend(self.scratch_lra_loudness.iter().copied().filter(|&v| v >= lra_rel));
                if self.scratch_kept.len() >= 2 {
                    // total_cmp avoids panics on hypothetical NaN inputs;
                    // partial_cmp().unwrap() would tear down the audio
                    // thread on an unexpected value.
                    self.scratch_kept.sort_by(|a, b| a.total_cmp(b));
                    let p10 = percentile(&self.scratch_kept, 0.10);
                    let p95 = percentile(&self.scratch_kept, 0.95);
                    self.snapshot.lra_lu = (p95 - p10).max(0.0);
                }
            }
        }
    }
}

impl LoudnessMeter {
    /// Weighted mean-square over the trailing `blocks` of 100 ms
    /// bins, including the in-progress 100 ms partial. Weights are
    /// the actual sample counts so the partial contributes
    /// proportionally to how far through the 100 ms bin we are.
    /// Returns `None` if there's no history at all.
    fn windowed_mean_sq_with_partial(&self, blocks: usize) -> Option<f32> {
        let closed = self.block_msq.len();
        let have_partial = self.block_count > 0;
        if closed == 0 && !have_partial {
            return None;
        }
        let spb = self.samples_per_block as f64;
        // Reserve the last slot for the partial when it exists so
        // the total window stays at `blocks * samples_per_block`.
        let take_closed = closed.min(blocks.saturating_sub(if have_partial { 1 } else { 0 }));
        let start = closed - take_closed;
        let mut total_sq = 0.0_f64;
        let mut total_n = 0.0_f64;
        for &msq in &self.block_msq[start..closed] {
            total_sq += msq as f64 * spb;
            total_n += spb;
        }
        if have_partial {
            total_sq += self.block_sum_sq_l + self.block_sum_sq_r;
            total_n += self.block_count as f64;
        }
        if total_n <= 0.0 {
            None
        } else {
            Some((total_sq / total_n) as f32)
        }
    }
}

/// Scratch buffers for out-of-thread integrated / LRA recompute. Owned by
/// the worker so repeated `compute_integrated_and_lra` calls stay
/// allocation-free. Sized on first use via the bin count.
#[derive(Default)]
pub struct IntegratedScratch {
    block_means: Vec<f32>,
    lra_loudness: Vec<f32>,
    kept: Vec<f32>,
}

/// Compute BS.1770-4 gated integrated loudness and EBU Tech 3342 LRA from a
/// chronological list of closed 100 ms channel-weighted mean-squares.
///
/// Returns `(integrated_lufs, lra_lu)`. Either value is `None` when the
/// sliding window isn't yet full enough (< 400 ms for integrated,
/// < 3 s for LRA) or the gating rejects all blocks (pure silence).
///
/// Pure function — callers own the `block_msq` and the scratch. Intended
/// for the off-thread loudness worker; the in-line meter path keeps its
/// own scratch inside `LoudnessMeter`.
pub fn compute_integrated_and_lra(
    block_msq: &[f32],
    scratch: &mut IntegratedScratch,
) -> (Option<f32>, Option<f32>) {
    let mut integrated_out = None;
    let mut lra_out = None;
    let n_bins = block_msq.len();
    if n_bins < MOMENTARY_BLOCKS {
        return (integrated_out, lra_out);
    }

    let stride = 1;
    let window = MOMENTARY_BLOCKS;
    let end = n_bins - window + 1;
    scratch.block_means.clear();
    let mut ungated_sum = 0.0_f64;
    let mut ungated_count = 0_usize;
    for start in (0..end).step_by(stride) {
        let m = mean_slice(&block_msq[start..start + window]);
        scratch.block_means.push(m);
        if loudness_from_mean_sq(m) >= ABSOLUTE_GATE_LUFS {
            ungated_sum += m as f64;
            ungated_count += 1;
        }
    }
    if ungated_count > 0 {
        let ungated_mean = (ungated_sum / ungated_count as f64) as f32;
        let rel_threshold = loudness_from_mean_sq(ungated_mean) - RELATIVE_GATE_LU;
        let mut gated_sum = 0.0_f64;
        let mut gated_count = 0_usize;
        for &m in &scratch.block_means {
            let lufs = loudness_from_mean_sq(m);
            if lufs >= ABSOLUTE_GATE_LUFS && lufs >= rel_threshold {
                gated_sum += m as f64;
                gated_count += 1;
            }
        }
        if gated_count > 0 {
            let gated_mean = (gated_sum / gated_count as f64) as f32;
            integrated_out = Some(loudness_from_mean_sq(gated_mean));
        }
    }

    if n_bins >= LRA_BLOCKS_PER_UPDATE {
        let lra_window = LRA_BLOCKS_PER_UPDATE;
        let lra_end = n_bins - lra_window + 1;
        scratch.lra_loudness.clear();
        let mut lra_ungated_sum = 0.0_f64;
        let mut lra_ungated_count = 0_usize;
        for start in (0..lra_end).step_by(stride) {
            let m = mean_slice(&block_msq[start..start + lra_window]);
            let lufs = loudness_from_mean_sq(m);
            if lufs >= ABSOLUTE_GATE_LUFS {
                lra_ungated_sum += m as f64;
                lra_ungated_count += 1;
                scratch.lra_loudness.push(lufs);
            }
        }
        if lra_ungated_count > 0 {
            let lra_ungated_mean = (lra_ungated_sum / lra_ungated_count as f64) as f32;
            let lra_rel = loudness_from_mean_sq(lra_ungated_mean) - LRA_RELATIVE_GATE_LU;
            scratch.kept.clear();
            scratch
                .kept
                .extend(scratch.lra_loudness.iter().copied().filter(|&v| v >= lra_rel));
            if scratch.kept.len() >= 2 {
                scratch.kept.sort_by(|a, b| a.total_cmp(b));
                let p10 = percentile(&scratch.kept, 0.10);
                let p95 = percentile(&scratch.kept, 0.95);
                lra_out = Some((p95 - p10).max(0.0));
            }
        }
    }

    (integrated_out, lra_out)
}

fn mean_slice(xs: &[f32]) -> f32 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut s = 0.0_f64;
    for &v in xs {
        s += v as f64;
    }
    (s / xs.len() as f64) as f32
}

fn loudness_from_mean_sq(m: f32) -> f32 {
    if m <= 1e-20 {
        MIN_LUFS
    } else {
        LKFS_OFFSET + 10.0 * m.log10()
    }
}

fn percentile(sorted: &[f32], p: f32) -> f32 {
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    let pos = (p.clamp(0.0, 1.0)) * (n as f32 - 1.0);
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(n - 1);
    let frac = pos - lo as f32;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen_sine(freq: f32, amp: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / sr).sin())
            .collect()
    }

    #[test]
    fn full_scale_1khz_sine_is_near_zero_lufs() {
        // 0 dBFS 1 kHz sine on BOTH channels: per-channel
        // mean-square = 0.5 (RMS = 1/√2), channel-weighted sum =
        // 1.0, → LKFS = −0.691 dB, plus a small pre-shelf bump at
        // 1 kHz → ~0 LUFS.
        let sr = 48_000.0;
        let mut meter = LoudnessMeter::new(sr);
        let samples = gen_sine(1000.0, 1.0, sr, (sr as usize) * 5);
        meter.process(&samples, &samples);
        let snap = meter.snapshot();
        assert!(
            (snap.short_term_lufs - 0.0).abs() < 1.0,
            "ST was {} LUFS, expected near 0",
            snap.short_term_lufs
        );
        assert!(
            snap.true_peak_max_dbtp > -1.0,
            "TP was {} dBTP, expected near 0",
            snap.true_peak_max_dbtp
        );
    }

    #[test]
    fn mono_channel_reads_lower_than_stereo() {
        // Same 0 dBFS sine on L only → one channel contributes,
        // should read about 3 LU lower than the stereo case.
        let sr = 48_000.0;
        let mut meter = LoudnessMeter::new(sr);
        let samples = gen_sine(1000.0, 1.0, sr, (sr as usize) * 5);
        let silence = vec![0.0_f32; samples.len()];
        meter.process(&samples, &silence);
        let snap = meter.snapshot();
        assert!(
            snap.short_term_lufs < -2.0 && snap.short_term_lufs > -4.0,
            "mono-channel ST was {} LUFS, expected ≈ -3",
            snap.short_term_lufs
        );
    }

    #[test]
    fn silence_stays_at_floor() {
        let sr = 48_000.0;
        let mut meter = LoudnessMeter::new(sr);
        let silence = vec![0.0_f32; sr as usize * 2];
        meter.process(&silence, &silence);
        let snap = meter.snapshot();
        assert_eq!(snap.integrated_lufs, MIN_LUFS);
        assert_eq!(snap.momentary_max_lufs, MIN_LUFS);
    }

    #[test]
    fn reset_clears_history() {
        let sr = 48_000.0;
        let mut meter = LoudnessMeter::new(sr);
        let samples = gen_sine(1000.0, 0.5, sr, sr as usize);
        meter.process(&samples, &samples);
        assert!(meter.snapshot().momentary_max_lufs > MIN_LUFS);
        meter.reset();
        assert_eq!(meter.snapshot().momentary_max_lufs, MIN_LUFS);
        assert_eq!(meter.snapshot().true_peak_max_dbtp, MIN_LUFS);
    }
}
