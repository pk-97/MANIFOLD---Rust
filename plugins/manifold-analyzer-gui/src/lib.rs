//! egui-based GUI for the Manifold Analyzer plugin.
//!
//! The spectrum line is now rendered by **manifold-gpu** — a WGSL fragment
//! shader writes into an IOSurface-backed texture, which egui's GL context
//! samples via a custom PaintCallback (no CPU round-trip). egui keeps
//! ownership of text labels, grid lines, and future controls; the "chrome
//! in egui, visuals in manifold-gpu" split.
//!
//! # TODO(manifold-gpu-migration)
//!
//! Grid lines are still egui. When we add the spectrogram, they'll move
//! into the shader alongside it. See MEMORY: `project_analyzer_gpu_migration.md`.
//!
//! # Audio thread ↔ GUI thread
//!
//! Audio thread publishes two spectra — Mid = (L+R)/2 and Side = (L-R)/2 —
//! via `try_lock` on `AnalyzerGuiShared::{mid_db, side_db}`, dropping the
//! update on contention. GUI thread briefly clones under each lock, then
//! uploads both into GPU-shared buffers (~8KB memcpy each, negligible).

mod cqt;
mod gl_paint;
mod gpu_bridge;
mod loudness_worker;
mod sample_ring;
mod spectrum_gpu;
mod spectrum_worker;

pub use loudness_worker::LoudnessWorker;

use gl_paint::{PainterState, QuadPainter, SharedPainterState};
use manifold_analyzer_dsp::{LoudnessSnapshot, MIN_DB};
use manifold_gpu::GpuDevice;
use nih_plug::prelude::*;
use nih_plug_egui::{EguiState, create_egui_editor, egui};
use sample_ring::SampleRing;
use spectrum_gpu::{DisplayConfig, SpectrumGpuRenderer};
use spectrum_worker::{CqtWorker, WorkerConfig};
use crossbeam_queue::ArrayQueue;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

// Initial render-target size; `SpectrumGpuRenderer::ensure_size` resizes every
// frame to match the current rect × pixels_per_point for pixel-perfect output.
const INITIAL_SPECTRUM_W: u32 = 900;
const INITIAL_SPECTRUM_H: u32 = 450;

// Hard caps on the GPU texture. 4K scenarios are well within this.
const MAX_SPECTRUM_W: u32 = 4096;
const MAX_SPECTRUM_H: u32 = 2048;

// Matched to SPAN preset: 10 Hz–25 kHz log, -90…-10 dB, +4.5 dB/oct tilt
// pivoted at 1 kHz, 1/12-oct frequency smoothing, filled display.
const DB_MIN: f32 = -90.0;
const DB_MAX: f32 = 0.0;

/// Fixed width of the right-side loudness meter panel (pixels).
/// Wide enough for a vertical meter column, the scale labels, and
/// readouts like "-14.4 LUFS" without truncation at the default
/// egui font size.
const LOUDNESS_PANEL_WIDTH: f32 = 180.0;
/// Reference frequency for the Flat/Pink/Tilted weighting slopes.
/// LUFS modes ignore this (the biquad response has its own pivot).
const SLOPE_REF_FREQ: f32 = 1000.0;
/// Auto-computed per frame: `-mean(weighting_db)` over the visible
/// freq range. Cancels the weighting curve's DC bias so the
/// display's overall level doesn't jump when switching between
/// weighting modes — only the *shape* of the weighting actually
/// colours the display.
fn weighting_align_offset(weighting: Weighting, freq_min: f32, freq_max: f32) -> f32 {
    if freq_max <= freq_min {
        return 0.0;
    }
    let stats = weighting_stats(weighting, freq_min, freq_max);
    -stats.mean
}

#[derive(Copy, Clone)]
struct WeightingStats {
    mean: f32,
    min: f32,
}

/// Returns mean/min of `weighting_db(f)` over a log-uniform grid in
/// [freq_min, freq_max]. Mean is used for align-offset (DC bias
/// removal). Min is used for the reference-curve overlay: pinning the
/// inverted weighting line to 0 dB at the freq where the weighting is
/// smallest gives a clean "equal-LUFS-contribution" reading — above
/// the line = this bin is driving loudness more than its balanced
/// share; below = room to push.
fn weighting_stats(weighting: Weighting, freq_min: f32, freq_max: f32) -> WeightingStats {
    if let Weighting::Flat = weighting {
        return WeightingStats { mean: 0.0, min: 0.0 };
    }
    match weighting {
        Weighting::Pink | Weighting::Tilted => {
            let slope = weighting.slope_db_per_oct();
            let gm = (freq_min * freq_max).sqrt();
            let mean = slope * (gm / SLOPE_REF_FREQ).log2();
            let v_lo = slope * (freq_min / SLOPE_REF_FREQ).log2();
            let v_hi = slope * (freq_max / SLOPE_REF_FREQ).log2();
            WeightingStats {
                mean,
                min: v_lo.min(v_hi),
            }
        }
        Weighting::Lufs | Weighting::LufsSubAdj => {
            let n = 64usize;
            let log_min = freq_min.ln();
            let log_max = freq_max.ln();
            let mut sum = 0.0_f32;
            let mut trough = f32::INFINITY;
            for i in 0..n {
                let t = (i as f32 + 0.5) / n as f32;
                let freq = (log_min + t * (log_max - log_min)).exp();
                let v = lufs_weighting_db(weighting, freq);
                sum += v;
                if v < trough {
                    trough = v;
                }
            }
            WeightingStats {
                mean: sum / n as f32,
                min: trough,
            }
        }
        Weighting::Flat => WeightingStats { mean: 0.0, min: 0.0 },
    }
}

/// Weighting curve value at a single frequency, matching the shader's
/// `weighting_db(freq)` function. Used by the reference-curve overlay
/// and the align-offset integration.
fn weighting_db_at(weighting: Weighting, freq: f32) -> f32 {
    match weighting {
        Weighting::Flat | Weighting::Pink | Weighting::Tilted => {
            weighting.slope_db_per_oct() * (freq / SLOPE_REF_FREQ).log2()
        }
        Weighting::Lufs | Weighting::LufsSubAdj => lufs_weighting_db(weighting, freq),
    }
}

fn lufs_weighting_db(weighting: Weighting, freq: f32) -> f32 {
    let pre = biquad_mag_db_48k(
        freq,
        1.535_124_8,
        -2.691_696_2,
        1.198_392_8,
        -1.690_659_3,
        0.732_480_8,
    );
    match weighting {
        Weighting::Lufs => {
            let rlb = biquad_mag_db_48k(
                freq,
                1.0,
                -2.0,
                1.0,
                -1.990_047_4,
                0.990_072_3,
            );
            pre + rlb
        }
        Weighting::LufsSubAdj => pre,
        _ => 0.0,
    }
}

fn biquad_mag_db_48k(freq: f32, b0: f32, b1: f32, b2: f32, a1: f32, a2: f32) -> f32 {
    let w = std::f32::consts::TAU * freq / 48_000.0;
    let (sw, cw) = w.sin_cos();
    let (s2w, c2w) = (2.0 * w).sin_cos();
    let num_re = b0 + b1 * cw + b2 * c2w;
    let num_im = -b1 * sw - b2 * s2w;
    let den_re = 1.0 + a1 * cw + a2 * c2w;
    let den_im = -a1 * sw - a2 * s2w;
    let num_mag2 = num_re * num_re + num_im * num_im;
    let den_mag2 = den_re * den_re + den_im * den_im;
    10.0 * (num_mag2.max(1e-30) / den_mag2.max(1e-30)).log10()
}
const SMOOTH_HALF_OCT_LOG2: f32 = 1.0 / 24.0; // 1/12-oct bandwidth → ±1/24 oct half-width
const FILL_ALPHA: f32 = 0.45;
// Colourmap dB range. Synchrosqueezing concentrates a main-lobe's worth
// of power into a single log bin, so peaks climb ~+4.5 dB vs raw VQT;
// with SS on we lift the ceiling to +10 dB so those peaks get headroom
// instead of saturating early. With SS off the raw VQT tops out near
// 0 dB — keeping the SS headroom would just dim the display, so we
// fall back to Vision 4X's 0 dB default.
const SPECTROGRAM_DB_MIN: f32 = -59.0;
const SPECTROGRAM_DB_MAX_SS: f32 = 10.0;
const SPECTROGRAM_DB_MAX_RAW: f32 = 0.0;
/// Sample-ring capacity in mono samples. Sized to tolerate ~1.3 s of
/// GUI stall at 48 kHz before the audio thread starts dropping — well
/// beyond anything we'd see in normal operation.
const SAMPLE_RING_CAPACITY: usize = 65_536;

/// Beats per bar assumed by Sync mode. Host time-signature isn't plumbed
/// yet; most music is in 4/4, so treat one "bar" as 4 beats. Revisit if
/// we surface host numerator.
const BEATS_PER_BAR: f32 = 4.0;

/// SPAN-style major frequency ticks (labeled, heavier grid line). These
/// get drawn + labeled whenever they fall inside the user's freq window.
const FREQ_MAJORS: &[f32] = &[
    10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0, 20_000.0,
];
/// Unlabeled minor ticks for finer readability inside each decade.
const FREQ_MINORS: &[f32] = &[
    30.0, 40.0, 60.0, 70.0, 80.0, 90.0, 300.0, 400.0, 600.0, 700.0, 800.0, 900.0, 3_000.0,
    4_000.0, 6_000.0, 7_000.0, 8_000.0, 9_000.0,
];

/// dB grid ticks for the top MS graph. Linear spacing: 20 dB majors,
/// 10 dB minors. Range must match DB_MIN..DB_MAX.
const DB_MAJORS: &[f32] = &[0.0, -20.0, -40.0, -60.0, -80.0];
const DB_MINORS: &[f32] = &[-10.0, -30.0, -50.0, -70.0];

/// Musical time factor selected in Sync mode. Matches Vision 4X's
/// "Factor" dropdown: the ratio that scales a bar. 1/1 = 1 bar per unit,
/// 1/2 = half bar, 2/1 = 2 bars, etc. Multiplied by the Multiplier to
/// get total bars visible on screen.
#[derive(Enum, PartialEq, Eq, Debug, Clone, Copy)]
pub enum SyncFactor {
    #[id = "1-4"]
    #[name = "1/4"]
    Quarter,
    #[id = "1-2"]
    #[name = "1/2"]
    Half,
    #[id = "1-1"]
    #[name = "1/1"]
    One,
    #[id = "2-1"]
    #[name = "2/1"]
    Two,
    #[id = "4-1"]
    #[name = "4/1"]
    Four,
}

impl SyncFactor {
    fn as_ratio(self) -> f32 {
        match self {
            SyncFactor::Quarter => 0.25,
            SyncFactor::Half => 0.5,
            SyncFactor::One => 1.0,
            SyncFactor::Two => 2.0,
            SyncFactor::Four => 4.0,
        }
    }
}

/// Vertical split between the top spectrum-curves region and the bottom
/// spectrogram. Values are the fraction of total height given to the
/// top region — 75% = tall curves, short spectrogram; 25% = the
/// opposite.
#[derive(Enum, PartialEq, Eq, Debug, Clone, Copy)]
pub enum TopRatio {
    #[id = "25"]
    #[name = "25/75"]
    P25,
    #[id = "40"]
    #[name = "40/60"]
    P40,
    #[id = "50"]
    #[name = "50/50"]
    P50,
    #[id = "60"]
    #[name = "60/40"]
    P60,
    #[id = "75"]
    #[name = "75/25"]
    P75,
}

impl TopRatio {
    fn fraction(self) -> f32 {
        match self {
            TopRatio::P25 => 0.25,
            TopRatio::P40 => 0.40,
            TopRatio::P50 => 0.50,
            TopRatio::P60 => 0.60,
            TopRatio::P75 => 0.75,
        }
    }
}

/// Frequency weighting applied to both the MS curves and the spectrogram
/// colormap. Flat/Pink/Tilted are plain linear-in-log-freq slopes; LUFS
/// is the full ITU-R BS.1770 K-weighting (HF shelf + 38 Hz HPF), which
/// is what LUFS meters use to model perceptual loudness. LUFS Sub Adj
/// keeps the HF shelf but drops the HPF so subsonic issues (rumble,
/// DC, plugin artifacts) remain visible instead of being attenuated
/// by 20+ dB — more useful for diagnosing mix problems than a strict
/// LUFS reading.
#[derive(Enum, PartialEq, Eq, Debug, Clone, Copy)]
pub enum Weighting {
    #[id = "flat"]
    #[name = "Flat"]
    Flat,
    #[id = "pink"]
    #[name = "Pink"]
    Pink,
    #[id = "tilted"]
    #[name = "Tilted"]
    Tilted,
    #[id = "lufs"]
    #[name = "LUFS"]
    Lufs,
    #[id = "lufs-sub"]
    #[name = "LUFS (Sub Adj)"]
    LufsSubAdj,
}

impl Weighting {
    /// Linear slope (dB/octave) used for Flat/Pink/Tilted modes. The
    /// LUFS modes ignore this; the shader picks them up via `mode_id`.
    fn slope_db_per_oct(self) -> f32 {
        match self {
            Weighting::Flat => 0.0,
            Weighting::Pink => 3.0,
            Weighting::Tilted => 4.5,
            _ => 0.0,
        }
    }

    fn mode_id(self) -> f32 {
        match self {
            Weighting::Flat => 0.0,
            Weighting::Pink => 1.0,
            Weighting::Tilted => 2.0,
            Weighting::Lufs => 3.0,
            Weighting::LufsSubAdj => 4.0,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Weighting::Flat => "Flat",
            Weighting::Pink => "Pink",
            Weighting::Tilted => "Tilted",
            Weighting::Lufs => "LUFS",
            Weighting::LufsSubAdj => "LUFS (Sub Adj)",
        }
    }
}

/// Optional overlay that plots the selected weighting curve directly
/// on the MS graph, scaled so its peak sits at 0 dB. Line high = this
/// frequency is loudness-efficient (+1 dB boost there drives LUFS a
/// lot); line low = weighting discounts this frequency, so any content
/// there eats peak budget without helping loudness.
#[derive(Enum, PartialEq, Eq, Debug, Clone, Copy)]
pub enum ReferenceCurve {
    #[id = "none"]
    #[name = "None"]
    None,
    #[id = "weighting"]
    #[name = "Weighting"]
    Weighting,
}

impl ReferenceCurve {
    fn label(self) -> &'static str {
        match self {
            ReferenceCurve::None => "None",
            ReferenceCurve::Weighting => "Weighting",
        }
    }
}

#[derive(Params)]
pub struct AnalyzerParams {
    #[persist = "editor-state"]
    pub editor_state: Arc<EguiState>,

    /// Lock the scrolling spectrogram to the host's bars/beats grid.
    /// When off, the spectrogram scrolls right-to-left at its native
    /// rate (one column per CQT hop).
    #[id = "sync"]
    pub sync: BoolParam,

    /// Factor side of the Sync time window. `window_bars = factor × multiplier`.
    #[id = "sync-factor"]
    pub sync_factor: EnumParam<SyncFactor>,

    /// Integer multiplier side of the Sync time window.
    #[id = "sync-mult"]
    pub sync_multiplier: IntParam,

    /// Master synchrosqueezing toggle. When off, the spectrogram shows
    /// the unmodified VQT power spectrum (thicker main lobes, no phase
    /// reassignment, no dropouts). When on, phase-advance reassigns
    /// energy toward the instantaneous frequency for tight tonal lines.
    #[id = "synchro"]
    pub synchrosqueeze: BoolParam,

    /// 3-frame coherence gate on top of synchrosqueezing. Rejects bins
    /// whose IF isn't stable across two consecutive frame boundaries —
    /// cleans up single-pixel transient scatter, but can stripe
    /// sustained notes when amplitude dips below the scatter gate for a
    /// single hop. Off by default because the striping is more visually
    /// jarring than the scatter it removes.
    #[id = "coherence"]
    pub coherence: BoolParam,

    /// Low edge of the display frequency range in Hz.
    #[id = "freq-min"]
    pub freq_min_hz: IntParam,

    /// High edge of the display frequency range in Hz. Clamped at
    /// render time to Nyquist (sr/2).
    #[id = "freq-max"]
    pub freq_max_hz: IntParam,

    /// Fractional split between the top spectrum-curves region and the
    /// bottom spectrogram.
    #[id = "top-ratio"]
    pub top_ratio: EnumParam<TopRatio>,

    /// Synchrosqueezing scatter gate in dB. Source bins below this
    /// power don't contribute to the squeezed scatter. Lower →
    /// transients survive; higher → cleaner on noise.
    #[id = "synchro-gate"]
    pub synchro_gate_db: FloatParam,

    /// Frequency weighting applied to both the MS curves and the
    /// spectrogram. See `Weighting` for what each mode does.
    #[id = "weighting"]
    pub weighting: EnumParam<Weighting>,

    /// Reference curve overlaid on the MS graph — shows the inverse of
    /// the current weighting so you can see which bins are under- or
    /// over-driving perceptual loudness.
    #[id = "ref-curve"]
    pub reference_curve: EnumParam<ReferenceCurve>,
}

impl AnalyzerParams {
    pub fn new() -> Self {
        Self {
            editor_state: EguiState::from_size(INITIAL_SPECTRUM_W, INITIAL_SPECTRUM_H),
            sync: BoolParam::new("Sync", false),
            sync_factor: EnumParam::new("Factor", SyncFactor::One),
            sync_multiplier: IntParam::new("Multiplier", 4, IntRange::Linear { min: 1, max: 16 }),
            synchrosqueeze: BoolParam::new("Synchrosqueeze", true),
            coherence: BoolParam::new("Coherence", false),
            freq_min_hz: IntParam::new("Min Hz", 10, IntRange::Linear { min: 10, max: 2000 }),
            freq_max_hz: IntParam::new(
                "Max Hz",
                22_000,
                IntRange::Linear { min: 1000, max: 25_000 },
            ),
            top_ratio: EnumParam::new("Split", TopRatio::P50),
            synchro_gate_db: FloatParam::new(
                "SS Gate",
                -75.0,
                FloatRange::Linear { min: -100.0, max: -30.0 },
            )
            .with_unit(" dB")
            .with_step_size(1.0),
            weighting: EnumParam::new("Weighting", Weighting::LufsSubAdj),
            reference_curve: EnumParam::new("Reference", ReferenceCurve::None),
        }
    }
}

impl Default for AnalyzerParams {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AnalyzerGuiShared {
    sample_rate_bits: AtomicU32,
    fft_size: AtomicUsize,
    /// Averaged (SPAN-style) Mid/Side spectra for the top curve.
    /// Accessed via `try_publish_*_db` (audio thread) and
    /// `try_read_*_db` (GUI thread); neither side blocks on the other.
    mid_db: Mutex<Vec<f32>>,
    side_db: Mutex<Vec<f32>>,
    /// Raw mid-channel audio samples for the CQT spectrogram. Audio
    /// thread pushes every sample; GUI thread drains and feeds the CQT
    /// pipeline. No FFT on the audio thread for this path.
    pub mid_sample_ring: SampleRing,
    /// Lock-free SPSC queue of closed 100 ms loudness-block z values.
    /// Produced by the audio thread's `LoudnessMeter::close_block`;
    /// consumed by the off-thread loudness worker which runs the
    /// O(N) BS.1770 integrated / LRA gating. Capacity: 256 bins ≈
    /// 25.6 s of buffer — overflow drops oldest (audio never blocks).
    pub loudness_block_queue: Arc<ArrayQueue<f32>>,
    /// Host transport snapshot, published from the audio thread each
    /// process block. `NaN` means "host did not provide this value" — we
    /// can only honour Sync mode when both `bpm_bits` and
    /// `beat_pos_bits` are finite.
    bpm_bits: AtomicU64,
    beat_pos_bits: AtomicU64,
    playing: AtomicBool,
    /// Loudness-meter readouts published by the audio thread each
    /// process block. All stored as f32 bits; `MIN_DB` means
    /// "not yet computed / silence".
    momentary_lufs_bits: AtomicU32,
    short_term_lufs_bits: AtomicU32,
    integrated_lufs_bits: AtomicU32,
    lra_lu_bits: AtomicU32,
    dr_lu_bits: AtomicU32,
    plr_lu_bits: AtomicU32,
    momentary_max_lufs_bits: AtomicU32,
    short_term_max_lufs_bits: AtomicU32,
    true_peak_max_dbtp_bits: AtomicU32,
    /// GUI increments this to request a meter reset; the audio
    /// thread compares against its last-seen value and resets on
    /// change. An atomic counter avoids the "miss the edge" problem
    /// of a plain boolean.
    reset_epoch: AtomicU32,
}

impl AnalyzerGuiShared {
    pub fn new(sample_rate: f32, fft_size: usize) -> Self {
        let num_bins = fft_size / 2;
        Self {
            sample_rate_bits: AtomicU32::new(sample_rate.to_bits()),
            fft_size: AtomicUsize::new(fft_size),
            mid_db: Mutex::new(vec![MIN_DB; num_bins]),
            side_db: Mutex::new(vec![MIN_DB; num_bins]),
            mid_sample_ring: SampleRing::new(SAMPLE_RING_CAPACITY),
            loudness_block_queue: Arc::new(ArrayQueue::new(256)),
            bpm_bits: AtomicU64::new(f64::NAN.to_bits()),
            beat_pos_bits: AtomicU64::new(f64::NAN.to_bits()),
            playing: AtomicBool::new(false),
            momentary_lufs_bits: AtomicU32::new(MIN_DB.to_bits()),
            short_term_lufs_bits: AtomicU32::new(MIN_DB.to_bits()),
            integrated_lufs_bits: AtomicU32::new(MIN_DB.to_bits()),
            lra_lu_bits: AtomicU32::new(0.0_f32.to_bits()),
            dr_lu_bits: AtomicU32::new(0.0_f32.to_bits()),
            plr_lu_bits: AtomicU32::new(0.0_f32.to_bits()),
            momentary_max_lufs_bits: AtomicU32::new(MIN_DB.to_bits()),
            short_term_max_lufs_bits: AtomicU32::new(MIN_DB.to_bits()),
            true_peak_max_dbtp_bits: AtomicU32::new(MIN_DB.to_bits()),
            reset_epoch: AtomicU32::new(0),
        }
    }

    pub fn set_sample_rate(&self, sr: f32) {
        self.sample_rate_bits.store(sr.to_bits(), Ordering::Relaxed);
    }

    pub fn sample_rate(&self) -> f32 {
        f32::from_bits(self.sample_rate_bits.load(Ordering::Relaxed))
    }

    pub fn fft_size(&self) -> usize {
        self.fft_size.load(Ordering::Relaxed)
    }

    pub fn set_transport(&self, bpm: Option<f64>, beat_pos: Option<f64>, playing: bool) {
        self.bpm_bits
            .store(bpm.unwrap_or(f64::NAN).to_bits(), Ordering::Relaxed);
        self.beat_pos_bits
            .store(beat_pos.unwrap_or(f64::NAN).to_bits(), Ordering::Relaxed);
        self.playing.store(playing, Ordering::Relaxed);
    }

    pub fn transport(&self) -> (Option<f64>, Option<f64>, bool) {
        let bpm = f64::from_bits(self.bpm_bits.load(Ordering::Relaxed));
        let beat = f64::from_bits(self.beat_pos_bits.load(Ordering::Relaxed));
        let playing = self.playing.load(Ordering::Relaxed);
        (
            if bpm.is_finite() && bpm > 0.0 { Some(bpm) } else { None },
            if beat.is_finite() { Some(beat) } else { None },
            playing,
        )
    }

    /// Audio-thread mailbox write for the averaged Mid spectrum. If the
    /// GUI happens to be reading (rare — it holds the lock only for an
    /// ~8 KB memcpy), the update is dropped. Returns `true` when the
    /// publish succeeded.
    pub fn try_publish_mid_db(&self, src: &[f32]) -> bool {
        if let Some(mut guard) = self.mid_db.try_lock() {
            guard.copy_from_slice(src);
            true
        } else {
            false
        }
    }

    pub fn try_publish_side_db(&self, src: &[f32]) -> bool {
        if let Some(mut guard) = self.side_db.try_lock() {
            guard.copy_from_slice(src);
            true
        } else {
            false
        }
    }

    /// GUI-thread mailbox read for the averaged Mid spectrum. Returns
    /// `false` when the audio thread is mid-write; callers keep their
    /// previous frame's values on miss.
    pub fn try_read_mid_db(&self, dst: &mut [f32]) -> bool {
        if let Some(guard) = self.mid_db.try_lock() {
            dst.copy_from_slice(&guard);
            true
        } else {
            false
        }
    }

    pub fn try_read_side_db(&self, dst: &mut [f32]) -> bool {
        if let Some(guard) = self.side_db.try_lock() {
            dst.copy_from_slice(&guard);
            true
        } else {
            false
        }
    }

    /// Full-snapshot publish — only used by the CLI / tests path where
    /// the meter computes integrated + LRA in-line. The plugin runtime
    /// uses `set_fast_loudness` (audio thread) + `set_integrated_lufs`
    /// / `set_lra_lu` (worker thread) to avoid clobbering.
    pub fn set_loudness(&self, s: LoudnessSnapshot) {
        self.momentary_lufs_bits
            .store(s.momentary_lufs.to_bits(), Ordering::Relaxed);
        self.short_term_lufs_bits
            .store(s.short_term_lufs.to_bits(), Ordering::Relaxed);
        self.integrated_lufs_bits
            .store(s.integrated_lufs.to_bits(), Ordering::Relaxed);
        self.lra_lu_bits.store(s.lra_lu.to_bits(), Ordering::Relaxed);
        self.dr_lu_bits.store(s.dr_lu.to_bits(), Ordering::Relaxed);
        self.plr_lu_bits.store(s.plr_lu.to_bits(), Ordering::Relaxed);
        self.momentary_max_lufs_bits
            .store(s.momentary_max_lufs.to_bits(), Ordering::Relaxed);
        self.short_term_max_lufs_bits
            .store(s.short_term_max_lufs.to_bits(), Ordering::Relaxed);
        self.true_peak_max_dbtp_bits
            .store(s.true_peak_max_dbtp.to_bits(), Ordering::Relaxed);
    }

    /// Audio-thread publish for the fast-updating readouts. Integrated
    /// and LRA are intentionally NOT touched — those are owned by the
    /// loudness worker so the audio thread's partial snapshot doesn't
    /// clobber them.
    pub fn set_fast_loudness(&self, s: LoudnessSnapshot) {
        self.momentary_lufs_bits
            .store(s.momentary_lufs.to_bits(), Ordering::Relaxed);
        self.short_term_lufs_bits
            .store(s.short_term_lufs.to_bits(), Ordering::Relaxed);
        self.dr_lu_bits.store(s.dr_lu.to_bits(), Ordering::Relaxed);
        self.plr_lu_bits.store(s.plr_lu.to_bits(), Ordering::Relaxed);
        self.momentary_max_lufs_bits
            .store(s.momentary_max_lufs.to_bits(), Ordering::Relaxed);
        self.short_term_max_lufs_bits
            .store(s.short_term_max_lufs.to_bits(), Ordering::Relaxed);
        self.true_peak_max_dbtp_bits
            .store(s.true_peak_max_dbtp.to_bits(), Ordering::Relaxed);
    }

    /// Worker-thread publish for integrated LUFS. Audio-thread DR / PLR
    /// readouts pick up the new value on their next update.
    pub fn set_integrated_lufs(&self, lufs: f32) {
        self.integrated_lufs_bits
            .store(lufs.to_bits(), Ordering::Relaxed);
    }

    /// Worker-thread publish for Loudness Range.
    pub fn set_lra_lu(&self, lra: f32) {
        self.lra_lu_bits.store(lra.to_bits(), Ordering::Relaxed);
    }

    /// Audio-thread read of the current integrated LUFS (for DR / PLR
    /// derivation in the meter's fast-path snapshot).
    pub fn integrated_lufs(&self) -> f32 {
        f32::from_bits(self.integrated_lufs_bits.load(Ordering::Relaxed))
    }

    pub fn loudness(&self) -> LoudnessSnapshot {
        let load = |a: &AtomicU32| f32::from_bits(a.load(Ordering::Relaxed));
        LoudnessSnapshot {
            momentary_lufs: load(&self.momentary_lufs_bits),
            short_term_lufs: load(&self.short_term_lufs_bits),
            integrated_lufs: load(&self.integrated_lufs_bits),
            lra_lu: load(&self.lra_lu_bits),
            dr_lu: load(&self.dr_lu_bits),
            plr_lu: load(&self.plr_lu_bits),
            momentary_max_lufs: load(&self.momentary_max_lufs_bits),
            short_term_max_lufs: load(&self.short_term_max_lufs_bits),
            true_peak_max_dbtp: load(&self.true_peak_max_dbtp_bits),
        }
    }

    /// GUI-side: increment to request a reset of running maxes /
    /// integrated / LRA on the audio thread. Momentary and
    /// short-term keep flowing.
    pub fn request_loudness_reset(&self) {
        self.reset_epoch.fetch_add(1, Ordering::Relaxed);
    }

    /// Audio-side: read current reset-request counter; process code
    /// compares against its last-seen value to detect an edge.
    pub fn loudness_reset_epoch(&self) -> u32 {
        self.reset_epoch.load(Ordering::Relaxed)
    }
}

struct EditorState {
    shared: Arc<AnalyzerGuiShared>,
    params: Arc<AnalyzerParams>,
    device: Option<GpuDevice>,
    spectrum: Option<SpectrumGpuRenderer>,
    /// CQT worker thread — spawned lazily on first paint so the ~500 ms
    /// kernel construction happens once we know the host sample rate.
    /// Dropped when `EditorState` drops (editor close / plugin destroy),
    /// joining the worker.
    worker: Option<CqtWorker>,
    quad: SharedPainterState,
    mid_scratch: Vec<f32>,
    side_scratch: Vec<f32>,
    /// Cached weighting-curve align offset so we don't re-integrate
    /// the BS.1770 biquads every frame — only when mode / freq range
    /// changes.
    align_offset_cache: AlignOffsetCache,
}

#[derive(Default)]
struct AlignOffsetCache {
    key: Option<(Weighting, f32, f32)>,
    value: f32,
}

fn align_offset_cached(
    cache: &mut AlignOffsetCache,
    weighting: Weighting,
    freq_min: f32,
    freq_max: f32,
) -> f32 {
    let key = (weighting, freq_min, freq_max);
    if cache.key == Some(key) {
        return cache.value;
    }
    let v = weighting_align_offset(weighting, freq_min, freq_max);
    cache.key = Some(key);
    cache.value = v;
    v
}

pub fn create_editor(
    params: Arc<AnalyzerParams>,
    shared: Arc<AnalyzerGuiShared>,
) -> Option<Box<dyn Editor>> {
    let num_bins = shared.fft_size() / 2;
    let egui_state = params.editor_state.clone();
    let state = EditorState {
        shared,
        params,
        device: None,
        spectrum: None,
        worker: None,
        quad: Arc::new(Mutex::new(PainterState::NotYet)),
        mid_scratch: vec![MIN_DB; num_bins],
        side_scratch: vec![MIN_DB; num_bins],
        align_offset_cache: AlignOffsetCache::default(),
    };
    create_egui_editor(
        egui_state,
        state,
        // Build callback runs on each editor spawn (open/close cycle). The
        // prior GL context is gone, so the cached `QuadPainter`'s GL handles
        // (program/VAO/texture) are now dangling — reset the lifecycle so
        // the next PaintCallback rebuilds against the fresh context.
        |_ctx, state: &mut EditorState| {
            *state.quad.lock() = PainterState::NotYet;
        },
        |ctx, setter, state| {
            egui::TopBottomPanel::top("analyzer-controls")
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(18, 22, 30)))
                .exact_height(26.0)
                .show(ctx, |ui| {
                    draw_controls(ui, state, setter);
                });
            egui::SidePanel::right("analyzer-loudness")
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(12, 15, 21)))
                .exact_width(LOUDNESS_PANEL_WIDTH)
                .resizable(false)
                .show(ctx, |ui| {
                    draw_loudness_panel(ui, state);
                });
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(8, 10, 14)))
                .show(ctx, |ui| {
                    draw_spectrum(ui, state);
                });
            // Continuous repaint — the loudness meter column reads
            // atomics that update every audio block, so we want a
            // real 60 fps rather than "every 16 ms the scheduler
            // gets around to it" (which can stretch to 30+ ms).
            ctx.request_repaint();
        },
    )
}

fn draw_spectrum(ui: &mut egui::Ui, state: &mut EditorState) {
    let sr = state.shared.sample_rate();
    let fft_size = state.shared.fft_size();
    let num_bins = fft_size / 2;
    if state.mid_scratch.len() != num_bins {
        state.mid_scratch.resize(num_bins, MIN_DB);
    }
    if state.side_scratch.len() != num_bins {
        state.side_scratch.resize(num_bins, MIN_DB);
    }

    // Target render-buffer size: rect × DPI, clamped.
    let ppp = ui.ctx().pixels_per_point();
    let available = ui.available_size();
    let phys_w = ((available.x * ppp).round() as u32).clamp(64, MAX_SPECTRUM_W);
    let phys_h = ((available.y * ppp).round() as u32).clamp(32, MAX_SPECTRUM_H);

    // Lazy GPU init.
    if state.device.is_none() {
        state.device = Some(GpuDevice::new());
    }
    // Spawn the CQT worker the first time we know the sample rate. The
    // worker owns the ~500 ms kernel construction; subsequent renders
    // just drain its output ring.
    if state.worker.is_none() && sr > 0.0 {
        let build = spectrum_gpu::cqt_build_params(sr);
        state.worker = Some(CqtWorker::spawn(sr, state.shared.clone(), build));
    }
    if state.spectrum.is_none() {
        if let (Some(device), Some(worker)) = (state.device.as_ref(), state.worker.as_ref()) {
            if let Some(spec) = SpectrumGpuRenderer::new(
                device,
                INITIAL_SPECTRUM_W,
                INITIAL_SPECTRUM_H,
                num_bins,
                worker.cqt_num_bins(),
            ) {
                state.spectrum = Some(spec);
            }
        }
    }

    // Resize the GPU texture to the current rect pixel size. If it rebuilt,
    // mark the GL-side painter for destroy+rebuild next PaintCallback (it
    // was bound to the now-dropped IOSurface).
    if let (Some(device), Some(spec)) = (state.device.as_ref(), state.spectrum.as_mut()) {
        if spec.ensure_size(device, phys_w, phys_h) {
            let mut lock = state.quad.lock();
            let prev = std::mem::replace(&mut *lock, PainterState::NotYet);
            *lock = match prev {
                PainterState::Ready(qp) => PainterState::PendingDestroy(qp),
                other => other,
            };
        }
    }

    // Copy latest averaged mid/side spectra for the curves. Try-lock both
    // sides so neither thread ever blocks the other — if the audio thread
    // is mid-write, we simply redraw the previous frame's spectrum.
    if let (Some(device), Some(spec), Some(worker)) = (
        state.device.as_ref(),
        state.spectrum.as_mut(),
        state.worker.as_ref(),
    ) {
        let _ = state.shared.try_read_mid_db(&mut state.mid_scratch);
        let _ = state.shared.try_read_side_db(&mut state.side_scratch);
        let ss_on = state.params.synchrosqueeze.value();
        let top_fraction = state.params.top_ratio.value().fraction();
        let weighting = state.params.weighting.value();
        let (freq_min_for_align, freq_max_for_align) = {
            let nyquist = sr * 0.5;
            let fmin_u = state.params.freq_min_hz.value() as f32;
            let fmax_u = state.params.freq_max_hz.value() as f32;
            let fmax = fmax_u.min(nyquist).max(fmin_u * 2.0);
            let fmin = fmin_u.min(fmax * 0.5).max(1.0);
            (fmin, fmax)
        };
        let align_offset = align_offset_cached(
            &mut state.align_offset_cache,
            weighting,
            freq_min_for_align,
            freq_max_for_align,
        );

        // Resolve sync state for display + worker.
        let (bpm_opt, beat_opt, _playing) = state.shared.transport();
        let sync_requested = state.params.sync.value();
        let factor_ratio = state.params.sync_factor.value().as_ratio();
        let multiplier = state.params.sync_multiplier.value() as f32;
        let beats_per_window = factor_ratio * multiplier * BEATS_PER_BAR;
        let sync_active = matches!(
            (sync_requested, bpm_opt, beat_opt),
            (true, Some(bpm), Some(_)) if bpm > 0.0 && beats_per_window > 0.0
        );

        spec.set_display(DisplayConfig {
            slope_db_per_oct: weighting.slope_db_per_oct(),
            slope_ref_freq: SLOPE_REF_FREQ,
            align_offset_db: align_offset,
            smooth_half_oct_log2: SMOOTH_HALF_OCT_LOG2,
            fill_alpha: FILL_ALPHA,
            spectrum_fraction: top_fraction,
            spectrogram_db_min: SPECTROGRAM_DB_MIN,
            spectrogram_db_max: if ss_on { SPECTROGRAM_DB_MAX_SS } else { SPECTROGRAM_DB_MAX_RAW },
            sync_mode: sync_active,
            weighting_mode: weighting.mode_id(),
        });

        // Hand current transport + display knobs to the worker. It reads
        // this once per hop; the config lock is uncontested here (only
        // the worker touches it otherwise).
        worker.post_config(WorkerConfig {
            sync_enabled: sync_active,
            bpm: bpm_opt.map(|v| v as f32).unwrap_or(0.0),
            beat_pos: beat_opt.unwrap_or(f64::NAN),
            beats_per_window,
            synchrosqueeze: ss_on,
            coherence: state.params.coherence.value(),
            synchro_gate_db: state.params.synchro_gate_db.value(),
        });

        // Drain completed CQT columns from the worker and write them into
        // the history storage buffer. `apply_column` handles history clears
        // on tempo change and advances the renderer's `write_col`.
        worker.drain_columns(|msg| spec.apply_column(&msg));

        let nyquist = sr * 0.5;
        let freq_min_user = state.params.freq_min_hz.value() as f32;
        let freq_max_user = state.params.freq_max_hz.value() as f32;
        let freq_max = freq_max_user.min(nyquist).max(freq_min_user * 2.0);
        let freq_min = freq_min_user.min(freq_max * 0.5).max(1.0);
        spec.render(
            device,
            &state.mid_scratch,
            &state.side_scratch,
            sr,
            freq_min,
            freq_max,
            DB_MIN,
            DB_MAX,
        );
    }

    // Allocate the rect for the whole spectrum view.
    let (rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let nyquist = sr * 0.5;
    let freq_min_user = state.params.freq_min_hz.value() as f32;
    let freq_max_user = state.params.freq_max_hz.value() as f32;
    let freq_max = freq_max_user.min(nyquist).max(freq_min_user * 2.0);
    let freq_min = freq_min_user.min(freq_max * 0.5).max(1.0);
    let top_fraction = state.params.top_ratio.value().fraction();

    // Spectrum-region rect (top). The spectrogram below is opaque, so
    // chrome underneath it gets hidden — spectrogram grid/labels are
    // drawn AFTER the paint callback further down.
    let spectrum_rect = egui::Rect::from_min_max(
        rect.min,
        egui::pos2(rect.max.x, rect.top() + rect.height() * top_fraction),
    );
    let spectrogram_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left(), spectrum_rect.bottom()),
        rect.max,
    );

    let grid_major = egui::Color32::from_gray(52);
    let grid_minor = egui::Color32::from_gray(32);
    let label_color = egui::Color32::from_gray(150);

    // 1. Grid lines underneath the spectrum curves (fills sit on top
    //    via the transparent shader, so grid shows through faintly).
    for &freq in FREQ_MINORS {
        if freq >= freq_min && freq <= freq_max {
            let x = freq_to_x(freq, freq_min, freq_max, spectrum_rect);
            painter.line_segment(
                [
                    egui::pos2(x, spectrum_rect.top()),
                    egui::pos2(x, spectrum_rect.bottom()),
                ],
                (1.0, grid_minor),
            );
        }
    }
    for &freq in FREQ_MAJORS {
        if freq >= freq_min && freq <= freq_max {
            let x = freq_to_x(freq, freq_min, freq_max, spectrum_rect);
            painter.line_segment(
                [
                    egui::pos2(x, spectrum_rect.top()),
                    egui::pos2(x, spectrum_rect.bottom()),
                ],
                (1.0, grid_major),
            );
        }
    }
    for &db in DB_MINORS {
        let y = db_to_y(db, DB_MIN, DB_MAX, spectrum_rect);
        painter.line_segment(
            [
                egui::pos2(spectrum_rect.left(), y),
                egui::pos2(spectrum_rect.right(), y),
            ],
            (1.0, grid_minor),
        );
    }
    for &db in DB_MAJORS {
        let y = db_to_y(db, DB_MIN, DB_MAX, spectrum_rect);
        painter.line_segment(
            [
                egui::pos2(spectrum_rect.left(), y),
                egui::pos2(spectrum_rect.right(), y),
            ],
            (1.0, grid_major),
        );
    }

    // 2. PaintCallback: GPU-rendered spectrum + spectrogram on top of the grid.
    if let Some(spec) = state.spectrum.as_ref() {
        let quad = state.quad.clone();
        let iosurface_addr = spec.iosurface() as usize;
        let w = spec.width();
        let h = spec.height();

        let callback = egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |_info, glow_painter| {
                let gl = glow_painter.gl();
                let mut lock = quad.lock();

                // Advance lifecycle if the Metal side has resized the IOSurface.
                let needs_build = match std::mem::replace(&mut *lock, PainterState::NotYet) {
                    PainterState::Ready(qp) => {
                        *lock = PainterState::Ready(qp);
                        false
                    }
                    PainterState::Failed => {
                        *lock = PainterState::Failed;
                        false
                    }
                    PainterState::NotYet => true,
                    PainterState::PendingDestroy(old) => {
                        old.destroy(gl);
                        true
                    }
                };
                if needs_build {
                    let iosurface = iosurface_addr as *mut std::ffi::c_void;
                    *lock = match QuadPainter::new(gl, iosurface, w, h) {
                        Some(qp) => PainterState::Ready(qp),
                        None => PainterState::Failed,
                    };
                }
                if let PainterState::Ready(qp) = &*lock {
                    qp.draw(gl);
                }
            })),
        };
        ui.painter().add(callback);
    } else {
        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(60, 15, 15));
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "manifold-gpu: init failed (see Console.app)",
            egui::FontId::monospace(12.0),
            egui::Color32::WHITE,
        );
        return;
    }

    // 2a. Reference curve (inverse weighting). Drawn after the grid
    //     but before text labels so the line is visible yet labels
    //     stay readable on top.
    draw_reference_curve(
        &painter,
        spectrum_rect,
        freq_min,
        freq_max,
        state.params.weighting.value(),
        state.params.reference_curve.value(),
    );

    // 3. Spectrum-region labels. Transparent shader lets these sit on
    //    top of the curves for readability.
    for &freq in FREQ_MAJORS {
        if freq >= freq_min && freq <= freq_max {
            let x = freq_to_x(freq, freq_min, freq_max, spectrum_rect);
            painter.text(
                egui::pos2(x + 3.0, spectrum_rect.bottom() - 3.0),
                egui::Align2::LEFT_BOTTOM,
                format_hz(freq),
                egui::FontId::monospace(10.0),
                label_color,
            );
        }
    }
    for &db in DB_MAJORS {
        let y = db_to_y(db, DB_MIN, DB_MAX, spectrum_rect);
        painter.text(
            egui::pos2(spectrum_rect.left() + 3.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{} dB", db as i32),
            egui::FontId::monospace(10.0),
            label_color,
        );
    }

    // 4. Spectrogram chrome — drawn AFTER the paint callback because
    //    the colourmap is opaque and would hide grid/labels drawn
    //    beneath.
    draw_spectrogram_chrome(
        &painter,
        spectrogram_rect,
        freq_min,
        freq_max,
        state,
    );
}

fn draw_reference_curve(
    painter: &egui::Painter,
    rect: egui::Rect,
    freq_min: f32,
    freq_max: f32,
    weighting: Weighting,
    mode: ReferenceCurve,
) {
    if matches!(mode, ReferenceCurve::None) || matches!(weighting, Weighting::Flat) {
        return;
    }
    // Inverse-weighting reference = the spectral shape that gives
    // EQUAL LUFS contribution at every frequency. Pinned so the line
    // touches 0 dB at the freq where the weighting is smallest (= the
    // freq that needs the MOST signal to register on LUFS).
    //
    // Reading:
    //   curve above line → this bin is driving loudness more than its
    //                      balanced share (pushing LUFS further)
    //   curve below line → room to push this bin up for more loudness,
    //                      whatever the frequency
    let stats = weighting_stats(weighting, freq_min, freq_max);
    let n = 128usize;
    let log_min = freq_min.ln();
    let log_max = freq_max.ln();
    let mut pts = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let freq = (log_min + t * (log_max - log_min)).exp();
        let ref_db = stats.min - weighting_db_at(weighting, freq);
        let x = rect.left() + t * rect.width();
        let y = db_to_y(ref_db, DB_MIN, DB_MAX, rect);
        pts.push(egui::pos2(x, y));
    }
    painter.add(egui::Shape::line(
        pts,
        egui::Stroke::new(1.5, egui::Color32::from_white_alpha(180)),
    ));
}

fn draw_spectrogram_chrome(
    painter: &egui::Painter,
    rect: egui::Rect,
    freq_min: f32,
    freq_max: f32,
    state: &EditorState,
) {
    if rect.height() < 8.0 {
        return;
    }
    let grid_over = egui::Color32::from_white_alpha(32);
    let grid_over_bold = egui::Color32::from_white_alpha(60);
    let label_color = egui::Color32::from_white_alpha(170);

    // Horizontal freq grid lines + right-anchored labels.
    for &freq in FREQ_MAJORS {
        if freq < freq_min || freq > freq_max {
            continue;
        }
        let t = (freq / freq_min).ln() / (freq_max / freq_min).ln();
        let y = rect.bottom() - t.clamp(0.0, 1.0) * rect.height();
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            (1.0, grid_over),
        );
        painter.text(
            egui::pos2(rect.right() - 4.0, y - 1.0),
            egui::Align2::RIGHT_BOTTOM,
            format_hz(freq),
            egui::FontId::monospace(10.0),
            label_color,
        );
    }

    // Sync-mode beat grid + beat numbers.
    let (bpm_opt, beat_opt, _) = state.shared.transport();
    let sync_on = state.params.sync.value();
    if !sync_on {
        return;
    }
    let (Some(_bpm), Some(beat_pos)) = (bpm_opt, beat_opt) else {
        return;
    };
    let beats_per_window = state.params.sync_factor.value().as_ratio()
        * state.params.sync_multiplier.value() as f32
        * BEATS_PER_BAR;
    if beats_per_window <= 0.0 {
        return;
    }

    // Adaptive tick density so we don't draw 256 labels on a 64-bar
    // window. Pick the smallest power-of-two beat step whose screen
    // spacing is at least ~24 px.
    let px_per_beat = rect.width() / beats_per_window;
    let step_beats = if px_per_beat >= 24.0 {
        1.0
    } else if px_per_beat >= 12.0 {
        2.0
    } else if px_per_beat >= 6.0 {
        BEATS_PER_BAR
    } else if px_per_beat >= 3.0 {
        BEATS_PER_BAR * 2.0
    } else {
        BEATS_PER_BAR * 4.0
    };

    // The window starts at the floored cycle boundary. beat_in_window
    // runs from 0 to beats_per_window; it maps linearly to x.
    let window_start_beat = (beat_pos / beats_per_window as f64).floor() * beats_per_window as f64;
    let mut i = 0.0_f32;
    while i <= beats_per_window + 1e-3 {
        let frac = i / beats_per_window;
        let x = rect.left() + frac * rect.width();
        let absolute_beat = window_start_beat + i as f64;
        let beat_idx_in_bar = absolute_beat.floor() as i64;
        let is_bar_boundary = beat_idx_in_bar.rem_euclid(BEATS_PER_BAR as i64) == 0;
        let (color, thickness) = if is_bar_boundary {
            (grid_over_bold, 1.5)
        } else {
            (grid_over, 1.0)
        };
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            (thickness, color),
        );
        // Beat number inside the current bar, 1-indexed. Skip if the
        // step is multiple bars (numbers would all read "1").
        if step_beats <= 2.0 {
            let beat_in_bar = beat_idx_in_bar.rem_euclid(BEATS_PER_BAR as i64) + 1;
            painter.text(
                egui::pos2(x + 3.0, rect.top() + 1.0),
                egui::Align2::LEFT_TOP,
                format!("{}", beat_in_bar),
                egui::FontId::monospace(10.0),
                label_color,
            );
        } else {
            // Multi-bar step: show bar number instead.
            let bar_num = beat_idx_in_bar.div_euclid(BEATS_PER_BAR as i64) + 1;
            painter.text(
                egui::pos2(x + 3.0, rect.top() + 1.0),
                egui::Align2::LEFT_TOP,
                format!("{}.1", bar_num),
                egui::FontId::monospace(10.0),
                label_color,
            );
        }
        i += step_beats;
    }
}

fn format_hz(freq: f32) -> String {
    if freq >= 1000.0 {
        let k = freq / 1000.0;
        if (k - k.round()).abs() < 0.01 {
            format!("{}k", k as i32)
        } else {
            format!("{:.1}k", k)
        }
    } else {
        format!("{}", freq as i32)
    }
}

/// Right-side loudness meter. Vertical scale spans both rows; a
/// filled column shows the momentary level, tick marks flank it on
/// both sides, a target band sits around −23 LUFS (EBU R128), and
/// the short-term-max line rides as a gutter hold. Readouts below
/// the column show short-term / integrated / LRA / DR / PLR plus
/// the M / ST / TP max trio.
fn draw_loudness_panel(ui: &mut egui::Ui, state: &mut EditorState) {
    let snap = state.shared.loudness();

    ui.vertical(|ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .button("Reset")
                .on_hover_text("Reset Integrated, LRA, and Max holds")
                .clicked()
            {
                state.shared.request_loudness_reset();
            }
            ui.label(
                egui::RichText::new("LUFS")
                    .color(egui::Color32::from_gray(190))
                    .size(12.0)
                    .strong(),
            );
        });
        ui.add_space(6.0);

        // Reserve ~210 px at the bottom for the 8-row readout block
        // (5 main rows + 4 px spacer + 3 max rows). Remainder is the
        // meter column.
        let total_avail = ui.available_size();
        let readout_height: f32 = 210.0;
        let column_height = (total_avail.y - readout_height - 8.0).max(180.0);
        let column_size = egui::vec2(total_avail.x, column_height);
        let (rect, _) = ui.allocate_exact_size(column_size, egui::Sense::hover());
        draw_meter_column(ui.painter_at(rect), rect, &snap);

        ui.add_space(8.0);
        draw_loudness_readouts(ui, &snap);
    });
}

/// Draw the vertical meter bar in `rect`. Column is centred in the
/// rect with tick marks flanking both sides and numeric labels on
/// the right. Top and bottom of the column are inset from the rect
/// edge so the `0` and `-54` labels don't get clipped at the panel
/// boundaries.
fn draw_meter_column(painter: egui::Painter, rect: egui::Rect, snap: &LoudnessSnapshot) {
    // Meter range: 0 LUFS at top, -54 LUFS at bottom.
    const METER_TOP: f32 = 0.0;
    const METER_BOTTOM: f32 = -54.0;
    const MAJOR_TICKS: &[f32] = &[0.0, -3.0, -6.0, -9.0, -18.0, -23.0, -27.0, -36.0, -45.0, -54.0];
    const LABELED_TICKS: &[f32] = &[0.0, -3.0, -6.0, -9.0, -18.0, -23.0, -27.0, -36.0, -45.0, -54.0];
    const TARGET_BAND: (f32, f32) = (-23.0, -18.0);

    // Vertical padding keeps the top (`0`) and bottom (`-54`) tick
    // labels fully inside `rect`. Label font is 10 px → 8 px margin
    // is enough for the text on either side of the column end.
    let v_pad = 8.0_f32;
    let top = rect.top() + v_pad;
    let bottom = rect.bottom() - v_pad;
    let height = (bottom - top).max(1.0);

    // Centre the column. Layout left-to-right across the rect:
    //   [ pad | left ticks | gap | column | gap | right ticks | gap | labels | pad ]
    let col_width = 22.0_f32;
    let left_tick_len = 6.0_f32;
    let right_tick_len = 6.0_f32;
    let tick_gap = 2.0_f32;
    let label_gap = 3.0_f32;
    // Label gutter fits a two-digit negative ("-54") at 10 px mono.
    let label_gutter = 24.0_f32;
    let content_width =
        left_tick_len + tick_gap + col_width + tick_gap + right_tick_len + label_gap + label_gutter;
    let content_left = rect.left() + ((rect.width() - content_width).max(0.0) * 0.5);
    let col_left = content_left + left_tick_len + tick_gap;
    let col_right = col_left + col_width;

    let col_rect = egui::Rect::from_min_max(
        egui::pos2(col_left, top),
        egui::pos2(col_right, bottom),
    );

    let lufs_to_y = |lufs: f32| -> f32 {
        let clamped = lufs.clamp(METER_BOTTOM, METER_TOP);
        let t = (clamped - METER_TOP) / (METER_BOTTOM - METER_TOP);
        top + t * height
    };

    // Column backdrop.
    painter.rect_filled(col_rect, 2.0, egui::Color32::from_rgb(26, 30, 38));

    // Target band.
    let target_top_y = lufs_to_y(TARGET_BAND.1);
    let target_bottom_y = lufs_to_y(TARGET_BAND.0);
    let target_rect = egui::Rect::from_min_max(
        egui::pos2(col_left, target_top_y),
        egui::pos2(col_right, target_bottom_y),
    );
    painter.rect_filled(
        target_rect,
        1.0,
        egui::Color32::from_rgba_unmultiplied(170, 60, 60, 110),
    );

    // Momentary fill (stepped colour keys).
    let m_lufs = snap.momentary_lufs;
    if m_lufs > METER_BOTTOM {
        let m_y = lufs_to_y(m_lufs);
        let fill_rect = egui::Rect::from_min_max(
            egui::pos2(col_left, m_y),
            egui::pos2(col_right, bottom),
        );
        let colour = meter_fill_colour(m_lufs);
        painter.rect_filled(fill_rect, 1.0, colour);
    }

    // Short-term max hold — extended past both ticks so it reads
    // as a sustained cap across the whole meter.
    if snap.short_term_max_lufs > METER_BOTTOM {
        let y = lufs_to_y(snap.short_term_max_lufs);
        painter.line_segment(
            [
                egui::pos2(col_left - left_tick_len - tick_gap, y),
                egui::pos2(col_right + right_tick_len + tick_gap, y),
            ],
            (1.5, egui::Color32::from_rgb(240, 240, 120)),
        );
    }

    // Flanking tick marks + right-side labels.
    let label_color = egui::Color32::from_gray(170);
    let target_label_color = egui::Color32::from_rgb(230, 180, 90);
    let tick_color = egui::Color32::from_gray(80);
    let left_tick_x1 = col_left - tick_gap;
    let left_tick_x0 = left_tick_x1 - left_tick_len;
    let right_tick_x0 = col_right + tick_gap;
    let right_tick_x1 = right_tick_x0 + right_tick_len;
    for &db in MAJOR_TICKS {
        let y = lufs_to_y(db);
        painter.line_segment(
            [egui::pos2(left_tick_x0, y), egui::pos2(left_tick_x1, y)],
            (1.0, tick_color),
        );
        painter.line_segment(
            [egui::pos2(right_tick_x0, y), egui::pos2(right_tick_x1, y)],
            (1.0, tick_color),
        );
        if LABELED_TICKS.contains(&db) {
            let is_target = (db - TARGET_BAND.0).abs() < 0.01;
            painter.text(
                egui::pos2(right_tick_x1 + label_gap, y),
                egui::Align2::LEFT_CENTER,
                format!("{}", db as i32),
                egui::FontId::monospace(10.0),
                if is_target {
                    target_label_color
                } else {
                    label_color
                },
            );
        }
    }
}

/// Momentary-column colour as a function of the current loudness.
/// Loud (≥ −9 LUFS) = saturated red; commercial-broadcast band
/// (−23…−14) = amber; below −36 = green. Stepped (not continuous)
/// so the eye reads bands, not a gradient.
fn meter_fill_colour(lufs: f32) -> egui::Color32 {
    if lufs >= -9.0 {
        egui::Color32::from_rgb(220, 60, 60)
    } else if lufs >= -14.0 {
        egui::Color32::from_rgb(220, 150, 60)
    } else if lufs >= -23.0 {
        egui::Color32::from_rgb(220, 200, 60)
    } else if lufs >= -36.0 {
        egui::Color32::from_rgb(120, 200, 90)
    } else {
        egui::Color32::from_rgb(70, 160, 120)
    }
}

fn draw_loudness_readouts(ui: &mut egui::Ui, snap: &LoudnessSnapshot) {
    let label_color = egui::Color32::from_gray(150);
    let value_color = egui::Color32::from_gray(230);
    let highlight_bg = egui::Color32::from_rgb(40, 60, 110);

    // Short-term / Integrated / LRA / DR / PLR — Integrated sits in
    // a highlighted row because that's the number most producers
    // are aiming at a specific target for.
    let row = |ui: &mut egui::Ui, label: &str, text: String, highlight: bool| {
        let bg = if highlight {
            highlight_bg
        } else {
            egui::Color32::TRANSPARENT
        };
        egui::Frame::new()
            .fill(bg)
            .inner_margin(egui::Margin::symmetric(4, 2))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(label).color(label_color).size(11.0),
                        )
                        .truncate(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(text).color(value_color).size(12.0).strong(),
                            )
                            .truncate(),
                        );
                    });
                });
            });
    };

    row(ui, "Short-term", fmt_lufs(snap.short_term_lufs), false);
    row(ui, "Integrated", fmt_lufs(snap.integrated_lufs), true);
    row(ui, "Range", fmt_lu(snap.lra_lu), false);
    row(ui, "Dynamic", fmt_lu(snap.dr_lu), false);
    row(ui, "PLR", fmt_lu(snap.plr_lu), false);
    ui.add_space(4.0);
    row(ui, "M Max", fmt_lufs(snap.momentary_max_lufs), false);
    row(ui, "ST Max", fmt_lufs(snap.short_term_max_lufs), false);
    row(ui, "TP Max", fmt_dbtp(snap.true_peak_max_dbtp), false);
}

fn fmt_lufs(v: f32) -> String {
    if v <= -120.0 {
        "-- LUFS".to_string()
    } else {
        format!("{:.1} LUFS", v)
    }
}

fn fmt_lu(v: f32) -> String {
    format!("{:.1} LU", v)
}

fn fmt_dbtp(v: f32) -> String {
    if v <= -120.0 {
        "-- dBTP".to_string()
    } else {
        format!("{:.1} dBTP", v)
    }
}

fn draw_controls(ui: &mut egui::Ui, state: &mut EditorState, setter: &ParamSetter) {
    let params = state.params.clone();
    let (bpm_opt, _beat, _playing) = state.shared.transport();
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;

        let mut sync_val = params.sync.value();
        let host_ready = bpm_opt.is_some();
        ui.add_enabled_ui(host_ready, |ui| {
            if ui.checkbox(&mut sync_val, "Sync").changed() {
                setter.begin_set_parameter(&params.sync);
                setter.set_parameter(&params.sync, sync_val);
                setter.end_set_parameter(&params.sync);
            }
        });

        ui.label("Factor");
        let mut factor_val = params.sync_factor.value();
        egui::ComboBox::from_id_salt("sync-factor")
            .selected_text(factor_label(factor_val))
            .width(58.0)
            .show_ui(ui, |ui| {
                for opt in [
                    SyncFactor::Quarter,
                    SyncFactor::Half,
                    SyncFactor::One,
                    SyncFactor::Two,
                    SyncFactor::Four,
                ] {
                    if ui
                        .selectable_value(&mut factor_val, opt, factor_label(opt))
                        .changed()
                    {
                        setter.begin_set_parameter(&params.sync_factor);
                        setter.set_parameter(&params.sync_factor, factor_val);
                        setter.end_set_parameter(&params.sync_factor);
                    }
                }
            });

        ui.label("Multiplier");
        let mut mult_val = params.sync_multiplier.value();
        if ui
            .add(egui::DragValue::new(&mut mult_val).range(1..=16).speed(0.1))
            .changed()
        {
            setter.begin_set_parameter(&params.sync_multiplier);
            setter.set_parameter(&params.sync_multiplier, mult_val);
            setter.end_set_parameter(&params.sync_multiplier);
        }

        ui.separator();

        let mut ss_val = params.synchrosqueeze.value();
        if ui.checkbox(&mut ss_val, "Synchro").changed() {
            setter.begin_set_parameter(&params.synchrosqueeze);
            setter.set_parameter(&params.synchrosqueeze, ss_val);
            setter.end_set_parameter(&params.synchrosqueeze);
        }

        let mut coh_val = params.coherence.value();
        ui.add_enabled_ui(ss_val, |ui| {
            if ui.checkbox(&mut coh_val, "Coherence").changed() {
                setter.begin_set_parameter(&params.coherence);
                setter.set_parameter(&params.coherence, coh_val);
                setter.end_set_parameter(&params.coherence);
            }
        });

        ui.label("Gate");
        let mut gate_val = params.synchro_gate_db.value();
        ui.add_enabled_ui(ss_val, |ui| {
            if ui
                .add(
                    egui::DragValue::new(&mut gate_val)
                        .range(-100.0..=-30.0)
                        .speed(0.5)
                        .suffix(" dB"),
                )
                .changed()
            {
                setter.begin_set_parameter(&params.synchro_gate_db);
                setter.set_parameter(&params.synchro_gate_db, gate_val);
                setter.end_set_parameter(&params.synchro_gate_db);
            }
        });

        ui.separator();

        ui.label("Min Hz");
        let mut fmin_val = params.freq_min_hz.value();
        if ui
            .add(egui::DragValue::new(&mut fmin_val).range(10..=2000).speed(1.0))
            .changed()
        {
            setter.begin_set_parameter(&params.freq_min_hz);
            setter.set_parameter(&params.freq_min_hz, fmin_val);
            setter.end_set_parameter(&params.freq_min_hz);
        }

        ui.label("Max Hz");
        let mut fmax_val = params.freq_max_hz.value();
        if ui
            .add(
                egui::DragValue::new(&mut fmax_val)
                    .range(1000..=25_000)
                    .speed(10.0),
            )
            .changed()
        {
            setter.begin_set_parameter(&params.freq_max_hz);
            setter.set_parameter(&params.freq_max_hz, fmax_val);
            setter.end_set_parameter(&params.freq_max_hz);
        }

        ui.label("Split");
        let mut ratio_val = params.top_ratio.value();
        egui::ComboBox::from_id_salt("top-ratio")
            .selected_text(top_ratio_label(ratio_val))
            .width(62.0)
            .show_ui(ui, |ui| {
                for opt in [
                    TopRatio::P25,
                    TopRatio::P40,
                    TopRatio::P50,
                    TopRatio::P60,
                    TopRatio::P75,
                ] {
                    if ui
                        .selectable_value(&mut ratio_val, opt, top_ratio_label(opt))
                        .changed()
                    {
                        setter.begin_set_parameter(&params.top_ratio);
                        setter.set_parameter(&params.top_ratio, ratio_val);
                        setter.end_set_parameter(&params.top_ratio);
                    }
                }
            });

        ui.label("Weighting");
        let mut weight_val = params.weighting.value();
        egui::ComboBox::from_id_salt("weighting")
            .selected_text(weight_val.label())
            .width(120.0)
            .show_ui(ui, |ui| {
                for opt in [
                    Weighting::Flat,
                    Weighting::Pink,
                    Weighting::Tilted,
                    Weighting::Lufs,
                    Weighting::LufsSubAdj,
                ] {
                    if ui.selectable_value(&mut weight_val, opt, opt.label()).changed() {
                        setter.begin_set_parameter(&params.weighting);
                        setter.set_parameter(&params.weighting, weight_val);
                        setter.end_set_parameter(&params.weighting);
                    }
                }
            });

        ui.label("Ref");
        let mut ref_val = params.reference_curve.value();
        egui::ComboBox::from_id_salt("ref-curve")
            .selected_text(ref_val.label())
            .width(88.0)
            .show_ui(ui, |ui| {
                for opt in [ReferenceCurve::None, ReferenceCurve::Weighting] {
                    if ui
                        .selectable_value(&mut ref_val, opt, opt.label())
                        .changed()
                    {
                        setter.begin_set_parameter(&params.reference_curve);
                        setter.set_parameter(&params.reference_curve, ref_val);
                        setter.end_set_parameter(&params.reference_curve);
                    }
                }
            });

        if let Some(bpm) = bpm_opt {
            ui.label(format!("{:.1} BPM", bpm));
        } else {
            ui.label("— BPM");
        }
    });
}

fn top_ratio_label(r: TopRatio) -> &'static str {
    match r {
        TopRatio::P25 => "25/75",
        TopRatio::P40 => "40/60",
        TopRatio::P50 => "50/50",
        TopRatio::P60 => "60/40",
        TopRatio::P75 => "75/25",
    }
}

fn factor_label(f: SyncFactor) -> &'static str {
    match f {
        SyncFactor::Quarter => "1/4",
        SyncFactor::Half => "1/2",
        SyncFactor::One => "1/1",
        SyncFactor::Two => "2/1",
        SyncFactor::Four => "4/1",
    }
}

fn freq_to_x(freq: f32, fmin: f32, fmax: f32, rect: egui::Rect) -> f32 {
    let t = (freq / fmin).ln() / (fmax / fmin).ln();
    rect.left() + t.clamp(0.0, 1.0) * rect.width()
}

fn db_to_y(db: f32, dmin: f32, dmax: f32, rect: egui::Rect) -> f32 {
    let t = (db - dmin) / (dmax - dmin);
    rect.bottom() - t.clamp(0.0, 1.0) * rect.height()
}
