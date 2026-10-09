//! Dedicated CQT worker thread.
//!
//! Audio thread pushes raw samples into the `SampleRing`; this thread drains
//! them, runs the CQT + synchrosqueezing pipeline, and pushes completed
//! columns into a bounded SPSC ring that the GUI drains each redraw.
//!
//! Why a separate thread:
//! 1. CQT is ~2.5 ms per display interval and the GUI used to run it inline. After a
//!    stall, the ring could buffer ~1 s of audio → 250 hops → a visible
//!    freeze. The worker decouples CQT cadence from redraw cadence.
//! 2. The worker emits at 187 Hz (5.33 ms / display interval) while dense
//!    variable-Q analysis runs inside each interval. Redraws at 60 Hz no
//!    longer gate analysis cadence, so the spectrogram stays current even
//!    when the GUI is throttled (window drag, DPI change, inactive tab).
//!
//! Threading contract: worker uses only atomics + lock-free rings + one
//! parking_lot mutex for the config snapshot. The audio thread stays
//! entirely out of this file — its only duty is pushing into `SampleRing`.

use crate::cqt::{CqtComplex, CqtTransform};
use crate::{AnalyzerGuiShared, SpectrogramSource, StereoSample};
use nih_plug_egui::EguiState;
use crossbeam_queue::ArrayQueue;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Sentinel dB for "silent" bins (no content / cleared history). Well
/// below `SPECTROGRAM_DB_MIN` so the shader's colormap renders it as
/// black. Matches `spectrum_gpu::FLOOR_DB`; kept local so the worker
/// doesn't depend on the renderer module.
const WORKER_FLOOR_DB: f32 = -140.0;

/// The legacy display cadence allowed 32 × 256 samples of catch-up. Dense
/// VQT analysis derives its corresponding analysis-hop budget from that same
/// audio duration, so a smaller analysis hop does not shorten recovery.
const MAX_HOPS_PER_DRAIN: usize = 32;
const LEGACY_HOP_SAMPLES: usize = 256;
const MAX_CATCHUP_SAMPLES: usize = MAX_HOPS_PER_DRAIN * LEGACY_HOP_SAMPLES;

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
    history_epoch: u64,
    pub col_idx: u32,
    /// dB per log bin. Length == `CqtWorker::cqt_num_bins()`.
    /// In Mid/Side modes this is the full single-channel spectrogram
    /// column; in L+R mode this is the Left channel.
    pub data: Vec<f32>,
    /// Recycled right-channel storage, used only when `secondary_active`.
    pub secondary_active: bool,
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
    pub causal: bool,
    pub threshold_rel: f32,
    /// Dense analysis hop. Derived from the shortest VQT kernel so every
    /// kernel advances by at most one quarter of its support.
    pub hop_samples: usize,
    /// Output cadence. Analysis runs at `hop_samples`, then per-bin peaks
    /// are reduced into one emitted column per display interval.
    pub display_hop_samples: usize,
    pub history_cols: u32,
}

pub struct CqtWorker {
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    column_ring: Arc<ArrayQueue<CqtColumnMsg>>,
    recycled: Arc<ArrayQueue<CqtColumnMsg>>,
    sample_rate: f32,
    config: Arc<Mutex<WorkerConfig>>,
    cqt_num_bins: usize,
    displayed_epoch: AtomicU64,
}

impl CqtWorker {
    /// Build the CQT on the calling thread (so the ~500 ms kernel
    /// construction happens on the GUI thread during first paint, not on
    /// the worker where it would delay first output) and spawn the worker.
    ///
    pub fn spawn(
        sample_rate: f32,
        shared: Arc<AnalyzerGuiShared>,
        editor: Arc<EguiState>,
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
            params.causal,
            params.threshold_rel,
        );
        let cqt_num_bins = cqt.num_bins();

        // Ring sized so the GUI can fall ~1 s behind at 187 hops/sec
        // without loss. Beyond that the producer's `force_push` wins —
        // oldest columns drop, matching the "visible gap, never a
        // freeze" behaviour of the stall-recovery cap.
        let column_ring = Arc::new(ArrayQueue::new(256));
        let recycled = Arc::new(ArrayQueue::new(258));
        for _ in 0..258 {
            recycled.push(CqtColumnMsg { history_epoch: 0, col_idx: 0, data: vec![WORKER_FLOOR_DB; cqt_num_bins],
                secondary_active: false, data2: Some(vec![WORKER_FLOOR_DB; cqt_num_bins]), clear_history_before: false }).ok();
        }
        let config = Arc::new(Mutex::new(WorkerConfig::OFF));
        let shutdown = Arc::new(AtomicBool::new(false));

        let thread = {
            let column_ring = column_ring.clone();
            let recycled = recycled.clone();
            let config = config.clone();
            let shutdown = shutdown.clone();
            thread::Builder::new()
                .name("manifold-analyzer-cqt".into())
                .spawn(move || {
                    let mut state = WorkerState::new(cqt, params, sample_rate, recycled.clone());
                    worker_loop(&mut state, shared, column_ring, config, shutdown, editor);
                })
                .expect("spawn manifold-analyzer-cqt thread")
        };

        Self {
            shutdown,
            thread: Some(thread),
            column_ring,
            recycled,
            sample_rate,
            config,
            cqt_num_bins,
            displayed_epoch: AtomicU64::new(u64::MAX),
        }
    }

    /// Publish a new config snapshot. Cheap — only the worker reads it.
    pub fn post_config(&self, cfg: WorkerConfig) {
        *self.config.lock() = cfg;
    }

    /// Drain all available completed columns. Callback is invoked in FIFO
    /// order. Returns the number drained.
    pub fn drain_columns<F: FnMut(&mut CqtColumnMsg)>(&self, mut f: F) -> usize {
        let mut count = 0;
        while let Some(mut col) = self.column_ring.pop() {
            col.clear_history_before = self.displayed_epoch.swap(col.history_epoch, Ordering::Relaxed) != col.history_epoch;
            f(&mut col);
            col.clear_history_before = false;
            let _ = self.recycled.push(col);
            count += 1;
        }
        count
    }

    pub fn sample_rate(&self) -> f32 { self.sample_rate }

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
    column_peak: Vec<f32>,
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
            column_peak: vec![WORKER_FLOOR_DB; cqt_num_bins],
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
        self.column_peak.fill(WORKER_FLOOR_DB);
    }

    fn reset_column_peak(&mut self) {
        self.column_peak.fill(WORKER_FLOOR_DB);
    }

    /// Invalidate phase and interpolation state after skipped analysis hops
    /// while preserving the rolling audio window for the next actual FFT.
    fn invalidate_analysis_history(&mut self) {
        self.have_prev_cqt = false;
        self.have_prev2_cqt = false;
        self.have_prev_cqt_out = false;
        self.reset_column_peak();
    }

    fn reduce_column_peak(&mut self) {
        for (peak, &value) in self.column_peak.iter_mut().zip(&self.cqt_out) {
            *peak = peak.max(value);
        }
    }
}

struct WorkerState {
    n_fft: usize,
    hop: usize,
    display_hop: usize,
    history_cols: u32,
    sample_rate: f32,

    /// Preallocated CPU VQT transform shared by the primary and secondary
    /// channels. Its FFT scratch is reused for every analysis hop.
    cqt: CqtTransform,
    primary: ChannelState,
    /// Secondary channel state. Allocated up front so switching to L+R
    /// mode at runtime never has to allocate on the worker thread.
    secondary: ChannelState,
    samples_since_last_hop: usize,
    samples_since_last_column: usize,
    analyzed_since_column: bool,
    synchro_power_scratch: Vec<f32>,

    internal_beat_pos: f64,
    have_internal_beat: bool,
    /// Running total of samples consumed from the shared L/R rings.
    /// Combined with the audio thread's published anchor `(beat_at_publish,
    /// pushed_at_publish)` lets us derive the sample-accurate host beat
    /// for the most recently processed sample at every emit — so the
    /// same Ableton song position always lands at the same spectrogram
    /// column regardless of when sync was enabled or how long the
    /// session has been running. Saturates instead of wrapping (a 64-bit
    /// counter at 48 kHz lasts ~12 million years; saturation is just for
    /// belt-and-braces).
    drained_total: u64,
    write_col: u32,
    last_applied_cfg: WorkerConfig,
    /// Pending-clear flag — set when config change invalidates history.
    /// Consumed on the next emitted column so the GUI clears first, then
    /// writes the fresh data.
    clear_pending: bool,
    recycled: Arc<ArrayQueue<CqtColumnMsg>>,
    generation: Option<u32>,
    history_epoch: u64,
}

impl WorkerState {
    fn new(cqt: CqtTransform, params: CqtBuildParams, sample_rate: f32, recycled: Arc<ArrayQueue<CqtColumnMsg>>) -> Self {
        let cqt_num_bins = cqt.num_bins();
        Self {
            n_fft: params.n_fft,
            hop: params.hop_samples,
            display_hop: params.display_hop_samples,
            history_cols: params.history_cols,
            sample_rate,
            cqt,
            primary: ChannelState::new(params.n_fft, cqt_num_bins),
            secondary: ChannelState::new(params.n_fft, cqt_num_bins),
            samples_since_last_hop: 0,
            samples_since_last_column: 0,
            analyzed_since_column: false,
            synchro_power_scratch: vec![0.0; cqt_num_bins],
            internal_beat_pos: 0.0,
            have_internal_beat: false,
            drained_total: 0,
            write_col: params.history_cols - 1,
            last_applied_cfg: WorkerConfig::OFF,
            clear_pending: true,
            recycled,
            generation: None,
            history_epoch: 0,
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
        // Toggling synchrosqueeze fundamentally changes the visual
        // character of every column (raw CQT vs phase-reassigned), so
        // mixing the two on screen is confusing. Clear so the user
        // sees a clean before/after instead of a half-and-half band.
        let synchro_changed = cfg.synchrosqueeze != self.last_applied_cfg.synchrosqueeze;
        if sync_changed || source_changed || synchro_changed {
            self.clear_pending = true;
            self.write_col = self.history_cols - 1;
            self.have_internal_beat = false;
            // A source switch swaps which channel is feeding the rolling
            // window, so the previous frame's CQT is no longer comparable
            // — reset per-channel phase + dB history on both buffers so
            // synchrosqueezing and power-domain lerp restart cleanly.
            self.primary.reset_history();
            self.secondary.reset_history();
            self.samples_since_last_column = 0;
            self.analyzed_since_column = false;
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

    /// Process stamped stereo frames. Runs CQT for the most recent
    /// hops, skipping older ones when the queue
    /// backed up. Pushes completed columns into the output ring
    /// (dropping oldest on overflow — matches the "gap, not freeze"
    /// policy).
    fn process(
        &mut self,
        samples: &[StereoSample],
        cfg: &WorkerConfig,
        ring: &ArrayQueue<CqtColumnMsg>,
    ) {
        let beats_per_column = if cfg.bpm > 0.0 {
            self.display_hop as f64 / self.sample_rate as f64 * cfg.bpm as f64 / 60.0
        } else {
            0.0
        };

        let pending = self.samples_since_last_hop + samples.len();
        let total_hops = pending / self.hop;
        let max_hops_per_drain = (MAX_CATCHUP_SAMPLES / self.hop).max(1);
        let skip_hops = total_hops.saturating_sub(max_hops_per_drain);
        let mut hops_seen: usize = 0;
        let two_channels = matches!(cfg.source, SpectrogramSource::LeftRight);
        let mut phase_invalidated = false;

        for sample in samples {
            if sample.sample_rate != self.sample_rate { continue; }
            if self.generation != Some(sample.generation) || sample.index != self.drained_total {
                self.primary.reset_history(); self.secondary.reset_history();
                self.samples_since_last_hop = 0;
                self.samples_since_last_column = 0;
                self.analyzed_since_column = false;
                self.clear_pending = true;
                self.generation = Some(sample.generation);
            }
            let (l, r) = (sample.left, sample.right);
            self.drained_total = sample.index;
            let anchor = if sample.beat.is_finite() && sample.bpm.is_finite() && sample.bpm > 0.0 {
                Some((sample.beat, sample.bpm, sample.index))
            } else { None };
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
            // Drained-sample counter advances per sample so emits inside
            // the loop see the right "newest sample index" for anchor
            // math. Saturates rather than wraps — a u64 at 48 kHz lasts
            // millennia, but cheap insurance.
            self.drained_total = self.drained_total.saturating_add(1);
            if self.samples_since_last_hop >= self.hop {
                self.samples_since_last_hop = 0;
                hops_seen += 1;

                if hops_seen <= skip_hops {
                    // Skip expensive CQT but keep the beat clock consistent.
                    // The next analyzed hop must not compare phase or output
                    // interpolation against a nonconsecutive frame.
                    if !phase_invalidated {
                        self.primary.invalidate_analysis_history();
                        self.secondary.invalidate_analysis_history();
                        self.clear_pending = true;
                        phase_invalidated = true;
                    }
                } else {
                    self.analyze_hop(cfg, two_channels);
                    self.analyzed_since_column = true;
                }
            }
            self.samples_since_last_column += 1;
            if self.samples_since_last_column >= self.display_hop {
                self.samples_since_last_column = 0;
                if self.analyzed_since_column {
                    self.emit_column(cfg, anchor, ring, two_channels);
                    self.analyzed_since_column = false;
                } else {
                    self.primary.reset_column_peak();
                    self.secondary.reset_column_peak();
                }
                self.internal_beat_pos += beats_per_column;
            }
        }
    }

    fn analyze_hop(&mut self, cfg: &WorkerConfig, two_channels: bool) {
        Self::run_channel_cqt(
            &mut self.cqt,
            &mut self.primary,
            cfg,
            self.hop,
            self.sample_rate,
            &mut self.synchro_power_scratch,
        );
        self.primary.reduce_column_peak();
        if two_channels {
            Self::run_channel_cqt(
                &mut self.cqt,
                &mut self.secondary,
                cfg,
                self.hop,
                self.sample_rate,
                &mut self.synchro_power_scratch,
            );
            self.secondary.reduce_column_peak();
        }
    }

    fn run_channel_cqt(
        cqt: &mut CqtTransform,
        ch: &mut ChannelState,
        cfg: &WorkerConfig,
        hop: usize,
        sample_rate: f32,
        synchro_scratch: &mut [f32],
    ) {
        // Copy rolling ring into oldest→newest linear order.
        let head = ch.rolling_head;
        let (tail, front) = ch.rolling.split_at(head);
        ch.scratch_audio[..front.len()].copy_from_slice(front);
        ch.scratch_audio[front.len()..].copy_from_slice(tail);

        cqt.process_complex(&ch.scratch_audio, &mut ch.cqt_complex);

        if cfg.synchrosqueeze && ch.have_prev_cqt {
            synchrosqueeze_into(
                cqt.center_freqs(),
                cqt.bandwidths_hz(),
                &ch.cqt_complex,
                &ch.prev_cqt_complex,
                &ch.prev2_cqt_complex,
                ch.have_prev2_cqt,
                cfg,
                hop,
                sample_rate,
                cqt.num_bins(),
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

    }

    fn emit_column(
        &mut self,
        cfg: &WorkerConfig,
        anchor: Option<(f64, f64, u64)>,
        ring: &ArrayQueue<CqtColumnMsg>,
        two_channels: bool,
    ) {
        // Decide target column(s). In sync mode we derive the host beat
        // for THIS display column's newest sample directly from the audio thread's
        // anchor — `(beat_at_publish, bpm, pushed_at_publish)` — so the
        // same Ableton song position lands at the same column every
        // time, with no drift across the session and no dependence on
        // when sync was first enabled. Falls back to the internal clock
        // only if the host hasn't reported transport yet (early frames
        // before any process buffer arrived).
        let prev_col = self.write_col;
        let beat_at_hop = if cfg.sync_enabled {
            match anchor {
                Some((beat_at_publish, bpm_anchor, pushed_at_publish)) => {
                    let beats_per_sample = bpm_anchor / 60.0 / self.sample_rate as f64;
                    // `drained_total` was just incremented to include the
                    // current display endpoint (loop-internal increment
                    // fires per sample); subtract 1 to get its index.
                    let newest_idx = self.drained_total.saturating_sub(1) as f64;
                    let samples_since_publish = newest_idx - pushed_at_publish as f64;
                    beat_at_publish + samples_since_publish * beats_per_sample
                }
                None => self.internal_beat_pos,
            }
        } else {
            self.internal_beat_pos
        };
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
                let hop_in_beats = self.display_hop as f64 / self.sample_rate as f64
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
        if clear_first { self.history_epoch = self.history_epoch.wrapping_add(1); }
        for i in 0..fill_count {
            let Some(mut msg) = self.recycled.pop().or_else(|| ring.pop()) else {
                // All buffers are temporarily held by the consumer. Keep the
                // discontinuity marker for the next column, without allocating.
                self.clear_pending |= clear_first;
                break;
            };
            msg.history_epoch = self.history_epoch;
            msg.secondary_active = two_channels;
            msg.col_idx = (start_col + i) % self.history_cols;
            msg.clear_history_before = clear_first && i == 0;
            let t = (i + 1) as f32 / fill_count as f32;
            if fill_count > 1 && self.primary.have_prev_cqt_out {
                lerp_power_db(&self.primary.prev_cqt_out, &self.primary.column_peak, t, &mut msg.data);
            } else { msg.data.copy_from_slice(&self.primary.column_peak); }
            if let Some(data) = msg.data2.as_mut() {
                if two_channels {
                    if fill_count > 1 && self.secondary.have_prev_cqt_out {
                        lerp_power_db(&self.secondary.prev_cqt_out, &self.secondary.column_peak, t, data);
                    } else { data.copy_from_slice(&self.secondary.column_peak); }
                }
            }
            if let Some(mut evicted) = ring.force_push(msg) {
                // Every column carries its epoch, so eviction cannot lose a reset.
                evicted.clear_history_before = false;
                let _ = self.recycled.push(evicted);
            }
        }

        self.primary
            .prev_cqt_out
            .copy_from_slice(&self.primary.column_peak);
        self.primary.have_prev_cqt_out = true;
        if two_channels {
            self.secondary
                .prev_cqt_out
                .copy_from_slice(&self.secondary.column_peak);
            self.secondary.have_prev_cqt_out = true;
        }
        self.primary.reset_column_peak();
        self.secondary.reset_column_peak();
    }
}

fn lerp_power_db(prev: &[f32], cur: &[f32], t: f32, out: &mut [f32]) {
    for ((&previous, &current), dst) in prev.iter().zip(cur).zip(out) {
        let p = 10.0_f32.powf(previous * 0.1) * (1.0 - t) + 10.0_f32.powf(current * 0.1) * t;
        *dst = if p > 1e-20 { 10.0 * p.log10() } else { WORKER_FLOOR_DB };
    }
}

#[allow(clippy::too_many_arguments)]
fn synchrosqueeze_into(
    center_freqs: &[f32],
    bandwidths: &[f32],
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

#[allow(clippy::too_many_arguments)]
fn worker_loop(
    state: &mut WorkerState,
    shared: Arc<AnalyzerGuiShared>,
    column_ring: Arc<ArrayQueue<CqtColumnMsg>>,
    config: Arc<Mutex<WorkerConfig>>,
    shutdown: Arc<AtomicBool>,
    editor: Arc<EguiState>,
) {
    let mut samples = Vec::with_capacity(65_536);
    while !shutdown.load(Ordering::Acquire) {
        samples.clear();
        shared.sample_ring.drain_into(&mut samples);
        if !editor.is_open() {
            while let Some(mut col) = column_ring.pop() {
                col.clear_history_before = false;
                let _ = state.recycled.push(col);
            }
            state.generation = None;
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        if samples.is_empty() { thread::sleep(IDLE_SLEEP); continue; }
        let cfg = *config.lock();
        state.apply_config(cfg);
        state.process(&samples, &cfg, &column_ring);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn analyzer_stamped_stream_and_recycled_columns() {
        for rate in [44100.0, 48000.0] {
            let params = CqtBuildParams { n_fft: 2048, fmin: 100.0, fmax: 8000.0,
                bpo: 12, gamma_lo: 10.0, gamma_hi: 20.0, gamma_transition: 200.0,
                causal: false, threshold_rel: 1e-4,
                hop_samples: 64, display_hop_samples: 256, history_cols: 32 };
            let cqt = CqtTransform::new(rate, params.n_fft, params.fmin, params.fmax,
                params.bpo, params.gamma_lo, params.gamma_hi, params.gamma_transition,
                params.causal, params.threshold_rel);
            let bins = cqt.num_bins();
            let pool = Arc::new(ArrayQueue::new(32));
            let mut pointers = Vec::new();
            for _ in 0..32 {
                let msg = CqtColumnMsg { history_epoch: 0, col_idx: 0, data: vec![0.0; bins],
                    secondary_active: false, data2: Some(vec![0.0; bins]), clear_history_before: false };
                pointers.push(msg.data.as_ptr()); pool.push(msg).ok();
            }
            let mut state = WorkerState::new(cqt, params, rate, pool.clone());
            let output = ArrayQueue::new(32);
            let cfg = WorkerConfig { source: SpectrogramSource::LeftRight, ..WorkerConfig::OFF };
            let samples: Vec<_> = (0..4096).map(|i| {
                let x = (std::f32::consts::TAU * 1000.0 * i as f32 / rate).sin();
                StereoSample { left: x, right: x, index: i, generation: 1,
                    sample_rate: rate, beat: i as f64 / rate as f64 * 2.0, bpm: 120.0 }
            }).collect();
            let start = std::time::Instant::now();
            state.process(&samples, &cfg, &output);
            eprintln!("small CPU CQT {rate}Hz: 16 hops in {:?}", start.elapsed());
            assert_eq!(state.drained_total, 4096);
            let mut output_count = 0;
            let mut saw_transient = false;
            while let Some(mut col) = output.pop() {
                output_count += 1;
                assert!(pointers.contains(&col.data.as_ptr()));
                assert_eq!(col.data, *col.data2.as_ref().unwrap());
                let peak = col.data.iter().enumerate().max_by(|a,b|a.1.total_cmp(b.1)).unwrap().0;
                if output_count >= 8 && col.data[peak] > -100.0 {
                    assert!((state.cqt.center_freqs()[peak] - 1000.0).abs() < 100.0);
                    saw_transient = true;
                }
                col.clear_history_before = false; pool.push(col).ok();
            }
            assert_eq!(output_count, 16, "one output column per 256 samples");
            assert!(saw_transient, "steady analysis lost the tone");
            // Reject an obsolete rate; then accept a reset/gap at the current rate.
            let mut sample = samples[0]; sample.sample_rate = rate + 1.0;
            state.process(&[sample], &cfg, &output);
            assert_eq!(state.drained_total, 4096);
            let mut next = samples[..256].to_vec();
            for (i,s) in next.iter_mut().enumerate() { s.index=9000+i as u64;s.generation=2; }
            state.process(&next, &cfg, &output);
            let col=output.pop().unwrap();assert!(col.clear_history_before);
            assert_eq!(state.drained_total,9256);assert_eq!(state.generation,Some(2));
        }
    }
    #[test]
    fn default_vqt_captures_short_high_frequency_bursts_between_display_hops() {
        let rate = 48000.0;
        let params = crate::spectrum_gpu::cqt_build_params(rate);
        let cqt = CqtTransform::new(rate, params.n_fft, params.fmin, params.fmax,
            params.bpo, params.gamma_lo, params.gamma_hi, params.gamma_transition,
            params.causal, params.threshold_rel);
        let bins = cqt.num_bins();
        let pool = Arc::new(ArrayQueue::new(8));
        for _ in 0..8 {
            pool.push(CqtColumnMsg { history_epoch: 0, col_idx: 0,
                data: vec![WORKER_FLOOR_DB; bins], secondary_active: false,
                data2: Some(vec![WORKER_FLOOR_DB; bins]), clear_history_before: false }).ok();
        }
        let mut state = WorkerState::new(cqt, params, rate, pool.clone());
        let output = ArrayQueue::new(8);
        let cfg = WorkerConfig::OFF;
        let mut weakest = 0.0_f32;
        for offset in (0..256).step_by(16) {
            let samples: Vec<_> = (0..512).map(|i| {
                let x = if (offset..offset+16).contains(&i) {
                    0.25 * (std::f32::consts::TAU * 18000.0 * i as f32 / rate).sin()
                } else { 0.0 };
                StereoSample { left:x, right:x, index:i as u64,
                    generation:offset as u32+1, sample_rate:rate, beat:f64::NAN, bpm:0.0 }
            }).collect();
            state.process(&samples, &cfg, &output);
            let mut peak = WORKER_FLOOR_DB;
            let mut columns = 0;
            while let Some(col) = output.pop() {
                for (&freq,&db) in state.cqt.center_freqs().iter().zip(&col.data) {
                    if (17000.0..19000.0).contains(&freq) { peak=peak.max(db); }
                }
                columns+=1;pool.push(col).ok();
            }
            assert_eq!(columns,2);
            assert!(peak > -27.0, "burst offset {offset} lost: {peak} dB");
            weakest=weakest.min(peak);
        }
        eprintln!("weakest 16-sample 18kHz burst: {weakest} dB");
    }

}
