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
mod gpu_cqt;
mod loudness_worker;
mod sample_ring;
mod spectrum_gpu;
mod spectrum_worker;

pub use loudness_worker::LoudnessWorker;

use gl_paint::{PainterState, QuadPainter, SharedPainterState};
use manifold_analyzer_dsp::{
    LoudnessSnapshot, MIN_DB, REF_FREQ_MAX, REF_FREQ_MIN, REF_POINTS, RefAnalysis,
    analyze_ref_file,
};
use manifold_gpu::GpuDevice;
use nih_plug::prelude::*;
use nih_plug_egui::{EguiState, create_egui_editor, egui};
use sample_ring::SampleRing;
use serde::{Deserialize, Serialize};
use spectrum_gpu::{
    CQT_BINS_PER_OCTAVE, CQT_FMIN_HZ, DisplayConfig, HISTORY_COLS, SpectrumGpuRenderer,
    WEIGHTING_LUT_SIZE,
};
use spectrum_worker::{CqtWorker, WorkerConfig};
use crossbeam_queue::ArrayQueue;
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

// Initial render-target size; `SpectrumGpuRenderer::ensure_size` resizes every
// frame to match the current rect × pixels_per_point for pixel-perfect output.
const INITIAL_SPECTRUM_W: u32 = 900;
const INITIAL_SPECTRUM_H: u32 = 450;

// Hard caps on the GPU texture. 4K scenarios are well within this.
const MAX_SPECTRUM_W: u32 = 4096;
const MAX_SPECTRUM_H: u32 = 2048;

// Default display window: 10 Hz–25 kHz log, -90…-10 dB, +4.5 dB/oct
// tilt pivoted at 1 kHz, 1/12-oct frequency smoothing, filled display.
const DB_MIN: f32 = -90.0;
const DB_MAX: f32 = 0.0;

// Right-column width and top/bottom split are runtime-adjustable via the
// grab handle at the 2×2 cross. See `AnalyzerGuiShared::right_column_width`
// and `top_fraction`.

/// Maximum number of reference-track slots persisted with the plugin.
/// Four covers typical mastering workflows (target + a couple of
/// commercial-release references).
pub const REF_SLOT_COUNT: usize = 4;

/// Display colours per slot. Two constraints stack here:
///
/// 1. Stay out of the warm half of the wheel so the lines don't sink
///    into the Mid (chartreuse green ≈ #B8FA61) or Side (orange-red
///    ≈ #F24D26) fills the GPU shader paints behind them.
/// 2. Stay distinct from each other when stacked. Keeping all four in
///    the cyan→magenta band looked tidy but made adjacent slots
///    indistinguishable; this palette spreads slots across hue *and*
///    lightness (cyan light, magenta mid, violet dark, near-white max)
///    so each curve reads as its own line even with all four loaded.
const REF_SLOT_COLORS: [[u8; 3]; REF_SLOT_COUNT] = [
    [50, 210, 255],   // cyan        — light, cool blue
    [255, 75, 205],   // hot magenta — saturated, warm pink (not red)
    [140, 100, 255],  // violet      — deep blue-purple, darker than the others
    [240, 240, 240],  // near-white  — neutral, maximum contrast vs every fill
];

/// Gaussian smoothing sigma (in log-grid points) per `FreqSmoothing`
/// mode. Bigger σ widens the effective bandwidth. Fixed-mode values
/// are tuned so adjacent grid points fall inside the kernel's main
/// lobe, killing the straight-line-segment artefacts that egui's
/// polyline rendering shows at low freq when σ is too small. ERB
/// mode returns `None` — the smoother computes σ per point from
/// the ERB critical-band curve.
fn ref_smooth_sigma_points(smoothing: FreqSmoothing) -> Option<f32> {
    match smoothing {
        FreqSmoothing::None => Some(0.0),
        FreqSmoothing::TwentyFourth => Some(3.0),
        FreqSmoothing::Twelfth => Some(10.0),
        FreqSmoothing::Sixth => Some(18.0),
        FreqSmoothing::Third => Some(30.0),
        FreqSmoothing::Erb => None,
    }
}
/// Truncation radius in σ multiples. 2.5 σ covers > 98 % of the
/// Gaussian energy; beyond that contributions don't change the
/// output meaningfully.
const REF_SMOOTH_RADIUS_SIGMAS: f32 = 2.5;
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
    -weighting_stats(weighting, freq_min, freq_max).mean
}

#[derive(Copy, Clone)]
struct WeightingStats {
    mean: f32,
    max: f32,
}

/// Returns mean/max of `weighting_db(f)` over a log-uniform grid in
/// [freq_min, freq_max]. Mean is used for align-offset (DC bias
/// removal on the shader side). Max is used by the reference-curve
/// overlay to pin the peak of the adjustment curve to 0 dB so the
/// positive half doesn't clip off the top of the MS plot.
fn weighting_stats(weighting: Weighting, freq_min: f32, freq_max: f32) -> WeightingStats {
    match weighting {
        Weighting::Flat => WeightingStats { mean: 0.0, max: 0.0 },
        Weighting::Pink | Weighting::Tilted => {
            let slope = weighting.slope_db_per_oct();
            let gm = (freq_min * freq_max).sqrt();
            let mean = slope * (gm / SLOPE_REF_FREQ).log2();
            let v_lo = slope * (freq_min / SLOPE_REF_FREQ).log2();
            let v_hi = slope * (freq_max / SLOPE_REF_FREQ).log2();
            WeightingStats {
                mean,
                max: v_lo.max(v_hi),
            }
        }
        Weighting::Lufs | Weighting::LufsSubAdj => {
            let n = 64usize;
            let log_min = freq_min.ln();
            let log_max = freq_max.ln();
            let mut sum = 0.0_f32;
            let mut peak = f32::NEG_INFINITY;
            for i in 0..n {
                let t = (i as f32 + 0.5) / n as f32;
                let freq = (log_min + t * (log_max - log_min)).exp();
                let v = lufs_weighting_db(weighting, freq);
                sum += v;
                if v > peak {
                    peak = v;
                }
            }
            WeightingStats {
                mean: sum / n as f32,
                max: peak,
            }
        }
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
const FILL_ALPHA: f32 = 0.45;
// Colourmap dB range. Synchrosqueezing concentrates a main-lobe's worth
// of power into a single log bin, so peaks climb ~+4.5 dB vs raw VQT;
// with SS on we lift the ceiling to +10 dB so those peaks get headroom
// instead of saturating early. With SS off the raw VQT tops out near
// 0 dB — keeping the SS headroom would just dim the display, so we
// fall back to a 0 dB ceiling.
const SPECTROGRAM_DB_MIN: f32 = -59.0;
const SPECTROGRAM_DB_MAX_SS: f32 = 10.0;
const SPECTROGRAM_DB_MAX_RAW: f32 = 0.0;
/// Gamma applied to the dB→colour mapping (values only; stored dB is
/// untouched). `< 1` brightens mids, `> 1` darkens. 0.7 lifts quiet
/// detail into the visible band without washing out peaks.
const SPECTROGRAM_GAMMA: f32 = 0.7;

/// Synchrosqueeze input-gate range. Single source of truth — used both
/// for the `FloatParam` definition and the `DragValue` widget clamp so
/// they can't drift out of sync.
const SS_GATE_DB_MIN: f32 = -100.0;
const SS_GATE_DB_MAX: f32 = -10.0;
/// Sample-ring capacity in mono samples. Sized to tolerate ~1.3 s of
/// GUI stall at 48 kHz before the audio thread starts dropping — well
/// beyond anything we'd see in normal operation.
const SAMPLE_RING_CAPACITY: usize = 65_536;

/// Beats per bar assumed by Sync mode. Host time-signature isn't plumbed
/// yet; most music is in 4/4, so treat one "bar" as 4 beats. Revisit if
/// we surface host numerator.
const BEATS_PER_BAR: f32 = 4.0;

/// Major frequency ticks (labeled, heavier grid line). These get
/// drawn + labeled whenever they fall inside the user's freq window.
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

#[derive(Enum, PartialEq, Eq, Debug, Clone, Copy)]
/// Length of the spectrogram's beat-locked horizontal window. Single
/// dropdown that replaces the previous `Factor × Multiplier` pair —
/// users pick the window directly ("4 bars") instead of computing it
/// from two interacting controls.
///
/// Values cover sub-bar zooms (one beat / half-bar) for transient
/// inspection through 64-bar windows for full-section overviews. The
/// `beats()` method is the single source of truth for every consumer
/// (worker, beat-grid chrome, sub-pixel column lerp).
pub enum SyncWindow {
    #[id = "beat"]
    #[name = "1 beat"]
    Beat,
    #[id = "halfbar"]
    #[name = "1/2 bar"]
    HalfBar,
    #[id = "bar1"]
    #[name = "1 bar"]
    Bar,
    #[id = "bar2"]
    #[name = "2 bars"]
    TwoBars,
    #[id = "bar4"]
    #[name = "4 bars"]
    FourBars,
    #[id = "bar8"]
    #[name = "8 bars"]
    EightBars,
    #[id = "bar16"]
    #[name = "16 bars"]
    SixteenBars,
    #[id = "bar32"]
    #[name = "32 bars"]
    ThirtyTwoBars,
    #[id = "bar64"]
    #[name = "64 bars"]
    SixtyFourBars,
}

impl SyncWindow {
    /// Window length in beats. Multiplied through `BEATS_PER_BAR`
    /// (currently 4) anywhere a bar count is needed.
    fn beats(self) -> f32 {
        match self {
            SyncWindow::Beat => 1.0,
            SyncWindow::HalfBar => BEATS_PER_BAR * 0.5,
            SyncWindow::Bar => BEATS_PER_BAR,
            SyncWindow::TwoBars => BEATS_PER_BAR * 2.0,
            SyncWindow::FourBars => BEATS_PER_BAR * 4.0,
            SyncWindow::EightBars => BEATS_PER_BAR * 8.0,
            SyncWindow::SixteenBars => BEATS_PER_BAR * 16.0,
            SyncWindow::ThirtyTwoBars => BEATS_PER_BAR * 32.0,
            SyncWindow::SixtyFourBars => BEATS_PER_BAR * 64.0,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SyncWindow::Beat => "1 beat",
            SyncWindow::HalfBar => "1/2 bar",
            SyncWindow::Bar => "1 bar",
            SyncWindow::TwoBars => "2 bars",
            SyncWindow::FourBars => "4 bars",
            SyncWindow::EightBars => "8 bars",
            SyncWindow::SixteenBars => "16 bars",
            SyncWindow::ThirtyTwoBars => "32 bars",
            SyncWindow::SixtyFourBars => "64 bars",
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
    #[name = "LUFS + Bass"]
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

    fn label(self) -> &'static str {
        match self {
            Weighting::Flat => "Flat",
            Weighting::Pink => "Pink",
            Weighting::Tilted => "Tilted",
            Weighting::Lufs => "LUFS",
            Weighting::LufsSubAdj => "LUFS + Bass",
        }
    }

    /// One-line hover description for the toolbar dropdown. Matches
    /// what the curve does to the visual, not the math behind it.
    fn tooltip(self) -> &'static str {
        match self {
            Weighting::Flat => {
                "No tilt - raw dB. Bass dominates visually because there's more energy at low freq."
            }
            Weighting::Pink => {
                "+3 dB/oct tilt - pink noise reads as a flat horizontal line. Standard for mixing."
            }
            Weighting::Tilted => {
                "+4.5 dB/oct tilt - like Pink but a bit more high-end emphasis."
            }
            Weighting::Lufs => {
                "ITU-R BS.1770 K-weighting (LUFS). Highlights what's actually driving perceived loudness."
            }
            Weighting::LufsSubAdj => {
                "K-weighting without the 38 Hz high-pass - keeps subsonic content visible for diagnosing rumble."
            }
        }
    }
}

/// Frequency smoothing bandwidth for the Mid/Side curves, L/R column,
/// and loaded reference envelopes. Fixed-width modes apply a constant
/// log-frequency window everywhere. ERB follows Moore & Glasberg's
/// critical-band curve — very wide at the low end (~2 oct at 20 Hz),
/// narrowing to ~1/6 oct above 5 kHz. Matches what the ear actually
/// integrates, which is what most mastering-grade analysers use to
/// hide the low-end stair-stepping behind.
#[derive(Enum, PartialEq, Eq, Debug, Clone, Copy)]
pub enum FreqSmoothing {
    #[id = "none"]
    #[name = "None"]
    None,
    #[id = "24"]
    #[name = "1/24 oct"]
    TwentyFourth,
    #[id = "12"]
    #[name = "1/12 oct"]
    Twelfth,
    #[id = "6"]
    #[name = "1/6 oct"]
    Sixth,
    #[id = "3"]
    #[name = "1/3 oct"]
    Third,
    #[id = "erb"]
    #[name = "ERB (Psychoacoustic)"]
    Erb,
}

impl FreqSmoothing {
    fn label(self) -> &'static str {
        match self {
            FreqSmoothing::None => "None",
            FreqSmoothing::TwentyFourth => "1/24 oct",
            FreqSmoothing::Twelfth => "1/12 oct",
            FreqSmoothing::Sixth => "1/6 oct",
            FreqSmoothing::Third => "1/3 oct",
            FreqSmoothing::Erb => "ERB (Psychoacoustic)",
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            FreqSmoothing::None => "Raw FFT bins - every spike is real, but low-end reads jagged.",
            FreqSmoothing::TwentyFourth => "1/24 octave - barely smoothed; preserves narrow peaks.",
            FreqSmoothing::Twelfth => "1/12 octave - default. Clean readout, peaks survive.",
            FreqSmoothing::Sixth => "1/6 octave - moderate; tonal balance over individual partials.",
            FreqSmoothing::Third => "1/3 octave - broad bands; reads like a mix-balance overview.",
            FreqSmoothing::Erb => {
                "Perceptual critical bands - wide at the low end, tight up top. Matches how the ear groups frequencies."
            }
        }
    }

    /// Fixed half-width in octaves (one-sided). `0.0` = no smoothing.
    /// Returns `None` for ERB mode (width is frequency-dependent).
    fn fixed_half_octaves(self) -> Option<f32> {
        match self {
            FreqSmoothing::None => Some(0.0),
            FreqSmoothing::TwentyFourth => Some(1.0 / 48.0),
            FreqSmoothing::Twelfth => Some(1.0 / 24.0),
            FreqSmoothing::Sixth => Some(1.0 / 12.0),
            FreqSmoothing::Third => Some(1.0 / 6.0),
            FreqSmoothing::Erb => None,
        }
    }

}

/// ERB-rate bandwidth (Moore & Glasberg 1983) at frequency `f` Hz.
/// Monotonic, always positive. At 20 Hz ≈ 27 Hz (2+ octaves symmetric),
/// at 1 kHz ≈ 133 Hz (~1/5 oct), at 10 kHz ≈ 1104 Hz (~1/9 oct).
fn erb_hz_at(f: f32) -> f32 {
    24.7 * (4.37 * f.max(0.0) * 1e-3 + 1.0)
}

/// Convert an ERB bandwidth at `f` Hz into a half-width in octaves.
/// `log2((f + erb/2) / f)` — the positive-side log distance. Used by
/// the CPU-side LR / ref smoothers in ERB mode. Shader does the same
/// math in WGSL.
fn erb_half_octaves_at(f: f32) -> f32 {
    if f <= 1e-3 {
        return 0.0;
    }
    let half_bw = erb_hz_at(f) * 0.5;
    (1.0 + half_bw / f).log2()
}

/// Chosen FFT size for the front-of-pipeline stereo analyser. Bigger
/// size = smaller bin width (better low-freq resolution) at the cost
/// of a longer time window (slower visual response, transients
/// diluted). Range covers the typical "live to detail" spread:
/// 2k snappy, 8k balanced default, 32k detailed offline-style
/// analysis. 64k is omitted on purpose — the 1.5 s window reads as
/// laggy on live material and the low-end win doesn't cover it.
#[derive(Enum, PartialEq, Eq, Debug, Clone, Copy)]
pub enum FftSize {
    #[id = "2k"]
    #[name = "2k"]
    K2,
    #[id = "4k"]
    #[name = "4k"]
    K4,
    #[id = "8k"]
    #[name = "8k"]
    K8,
    #[id = "16k"]
    #[name = "16k"]
    K16,
    #[id = "32k"]
    #[name = "32k"]
    K32,
}

impl FftSize {
    pub fn samples(self) -> usize {
        match self {
            FftSize::K2 => 2_048,
            FftSize::K4 => 4_096,
            FftSize::K8 => 8_192,
            FftSize::K16 => 16_384,
            FftSize::K32 => 32_768,
        }
    }

    fn label(self) -> &'static str {
        match self {
            FftSize::K2 => "2k",
            FftSize::K4 => "4k",
            FftSize::K8 => "8k",
            FftSize::K16 => "16k",
            FftSize::K32 => "32k",
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            FftSize::K2 => "2048 - fastest response, coarsest pitch resolution.",
            FftSize::K4 => "4096 - balanced default for live monitoring.",
            FftSize::K8 => "8192 - finer pitch detail, slower transient response.",
            FftSize::K16 => "16384 - mastering-grade resolution; visible smear on transients.",
            FftSize::K32 => "32768 - maximum bin count; only for static-tone analysis.",
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
    #[name = "Weighting Curve"]
    Weighting,
}

impl ReferenceCurve {
    fn label(self) -> &'static str {
        match self {
            ReferenceCurve::None => "None",
            ReferenceCurve::Weighting => "Weighting Curve",
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            ReferenceCurve::None => "No overlay on the MS graph.",
            ReferenceCurve::Weighting => {
                "Plots the inverse of the active weighting curve, peak-aligned to 0 dB. \
                 Line high = perceptually loud band; line low = penalised band."
            }
        }
    }
}

/// Channel(s) feeding the spectrogram. `Mid` and `Side` route a single
/// derived stream into the CQT pipeline; `LeftRight` runs two CQT passes
/// per hop and the renderer splits the spectrogram region in half (top =
/// L, bottom = R) so the channels can be compared side-by-side.
///
/// Stored as `u8` in `AnalyzerGuiShared::spectrogram_source_bits`; values
/// outside `0..=2` are coerced back to `Mid` on read so a corrupted
/// atomic never lands the worker in an invalid branch.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SpectrogramSource {
    Mid = 0,
    Side = 1,
    LeftRight = 2,
}

impl SpectrogramSource {
    fn from_bits(b: u8) -> Self {
        match b {
            1 => SpectrogramSource::Side,
            2 => SpectrogramSource::LeftRight,
            _ => SpectrogramSource::Mid,
        }
    }

    /// Compact chip label for the toolbar buttons. `L | R` is rendered
    /// rather than `L+R` so the visual reads as "two channels side by
    /// side" instead of "summed" — `L+R` would suggest a mono-mix
    /// spectrogram, which is what `M` already does.
    fn chip_label(self) -> &'static str {
        match self {
            SpectrogramSource::Mid => "M",
            SpectrogramSource::Side => "S",
            SpectrogramSource::LeftRight => "L | R",
        }
    }
}

/// One loaded reference track. Holds just enough to re-render the band
/// without re-decoding the source file: the pre-computed percentile
/// envelopes, the source's integrated LUFS for gain-match, and an
/// optional LAME lowpass cutoff so codec brickwalls don't mislead.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RefSlot {
    /// File name (no path). Empty string → slot is unused.
    pub name: String,
    /// Analysis result. `None` if the slot is empty; `Some` once a file
    /// has been loaded and analysed.
    pub analysis: Option<RefAnalysis>,
    /// GUI visibility toggle. Defaults to `true` on new load.
    pub visible: bool,
}

impl RefSlot {
    pub fn is_loaded(&self) -> bool {
        self.analysis.is_some()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefSlots {
    pub slots: [RefSlot; REF_SLOT_COUNT],
}

impl Default for RefSlots {
    fn default() -> Self {
        Self {
            slots: core::array::from_fn(|_| RefSlot::default()),
        }
    }
}

#[derive(Params)]
pub struct AnalyzerParams {
    #[persist = "editor-state"]
    pub editor_state: Arc<EguiState>,

    /// Loaded reference tracks (up to `REF_SLOT_COUNT`). Persisted with
    /// the plugin state so the user doesn't have to re-load refs each
    /// session. Only the envelopes + integrated LUFS are stored, not
    /// the source audio.
    #[persist = "ref-slots"]
    pub ref_slots: Arc<RwLock<RefSlots>>,

    /// Lock the scrolling spectrogram to the host's bars/beats grid.
    /// When off, the spectrogram scrolls right-to-left at its native
    /// rate (one column per CQT hop).
    #[id = "sync"]
    pub sync: BoolParam,

    /// Length of the beat-locked spectrogram window. Replaces the old
    /// `Factor × Multiplier` pair with a single dropdown so the user
    /// picks the visible time-span directly rather than multiplying two
    /// numbers in their head.
    #[id = "sync-window"]
    pub sync_window: EnumParam<SyncWindow>,

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

    /// Half-range of the L/R SPL column in dB. Each side spans 0 dB
    /// (outer edge, loudest) down to −this value (centerline, silence).
    /// 20..90 dB covers "mastering zoom on the top 20 dB" through
    /// "full analyser floor matching the MS plot".
    #[id = "lr-range"]
    pub lr_range_db: IntParam,

    /// Frequency-domain smoothing bandwidth applied to MS curves,
    /// L/R column, and loaded reference envelopes. ERB mode uses a
    /// perceptually-shaped variable-width window.
    #[id = "freq-smooth"]
    pub freq_smoothing: EnumParam<FreqSmoothing>,

    /// FFT size for the audio-thread stereo analyser. Change rebuilds
    /// the FFT plan + resizes the shared mailboxes on the audio thread
    /// (brief glitch on change, stable otherwise).
    #[id = "fft-size"]
    pub fft_size: EnumParam<FftSize>,
}

impl AnalyzerParams {
    pub fn new() -> Self {
        Self {
            editor_state: EguiState::from_size(INITIAL_SPECTRUM_W, INITIAL_SPECTRUM_H),
            ref_slots: Arc::new(RwLock::new(RefSlots::default())),
            sync: BoolParam::new("Sync", true),
            sync_window: EnumParam::new("Window", SyncWindow::FourBars),
            synchrosqueeze: BoolParam::new("Synchrosqueeze", false),
            coherence: BoolParam::new("Coherence", false),
            freq_min_hz: IntParam::new("Min Hz", 20, IntRange::Linear { min: 10, max: 2000 }),
            freq_max_hz: IntParam::new(
                "Max Hz",
                20_000,
                IntRange::Linear { min: 1000, max: 25_000 },
            ),
            synchro_gate_db: FloatParam::new(
                "SS Gate",
                -35.0,
                FloatRange::Linear { min: SS_GATE_DB_MIN, max: SS_GATE_DB_MAX },
            )
            .with_unit(" dB")
            .with_step_size(1.0),
            weighting: EnumParam::new("Weighting", Weighting::Pink),
            reference_curve: EnumParam::new("Reference", ReferenceCurve::None),
            lr_range_db: IntParam::new("L/R Range", 10, IntRange::Linear { min: 5, max: 60 }),
            freq_smoothing: EnumParam::new("Smoothing", FreqSmoothing::Erb),
            fft_size: EnumParam::new("FFT Size", FftSize::K4),
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
    /// Averaged Mid/Side spectra for the top curve. Accessed via
    /// `try_publish_*_db` (audio thread) and `try_read_*_db` (GUI
    /// thread); neither side blocks on the other.
    mid_db: Mutex<Vec<f32>>,
    side_db: Mutex<Vec<f32>>,
    /// Averaged Left / Right spectra for the L/R comparison column.
    /// Same mailbox pattern as mid/side; audio thread publishes when it
    /// finishes a frame, GUI thread reads each repaint.
    left_db: Mutex<Vec<f32>>,
    right_db: Mutex<Vec<f32>>,
    /// Symmetrically-smoothed L / R magnitudes for the balance line.
    /// Peak-hold values from `left_db` / `right_db` make the balance
    /// stick at whichever channel peaked most recently; these track
    /// the current signal instead.
    left_balance_db: Mutex<Vec<f32>>,
    right_balance_db: Mutex<Vec<f32>>,
    /// Per-bin stereo correlation in [-1, 1]. Same mailbox pattern as
    /// the spectra. Drives the colour strip in the L/R cell.
    correlation_bins: Mutex<Vec<f32>>,
    /// Raw L/R audio samples for the CQT spectrogram. Audio thread pushes
    /// every sample to both rings; the worker drains them and derives
    /// Mid/Side on demand based on the current `SpectrogramSource`. Two
    /// rings instead of pre-mixing to Mid lets the user switch between
    /// Mid / Side / L+R modes without re-running the audio thread or
    /// allocating per-channel buffers there.
    pub left_sample_ring: SampleRing,
    pub right_sample_ring: SampleRing,
    /// Spectrogram source mode. Drives both the worker (which channel(s)
    /// to CQT) and the renderer (single full-height vs L+R stacked
    /// split). Stored as `u8` so we can use a single relaxed atomic load
    /// per frame on the GUI thread.
    spectrogram_source_bits: AtomicU8,
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
    sample_peak_db_bits: AtomicU32,
    sample_peak_max_db_bits: AtomicU32,
    rms_db_bits: AtomicU32,
    rms_max_db_bits: AtomicU32,
    correlation_bits: AtomicU32,
    elapsed_secs_bits: AtomicU32,
    /// GUI increments this to request a meter reset; the audio
    /// thread compares against its last-seen value and resets on
    /// change. An atomic counter avoids the "miss the edge" problem
    /// of a plain boolean.
    reset_epoch: AtomicU32,
    /// Bit N set → reference slot N has an analysis worker thread
    /// in flight. GUI flips these on when spawning the worker and
    /// off when the worker writes its result back into the params
    /// slot. Used only to draw a "…analysing" indicator; never
    /// gates audio-thread behaviour.
    ref_analyzing_mask: AtomicU8,
    /// Layout split state driven by the 2×2 grab handle. Horizontal:
    /// width of the right column in logical pixels (MS/spectrogram
    /// takes the rest). Vertical: fraction of the remaining height
    /// below the two top bars given to the top row (MS plot + L/R
    /// column). Both atomics are the single source of truth — no
    /// host-visible param, no automation lane.
    right_column_width_bits: AtomicU32,
    top_fraction_bits: AtomicU32,
}

/// Defaults the grab handle snaps back to on double-click. The drag
/// only clamps to keep the handle on-screen; any panel may collapse
/// to zero to let one of the four figures go full-window.
const RIGHT_COLUMN_WIDTH_DEFAULT: f32 = 200.0;
const TOP_FRACTION_DEFAULT: f32 = 0.5;

fn publish_if_size_matches(mailbox: &Mutex<Vec<f32>>, src: &[f32]) -> bool {
    let Some(mut guard) = mailbox.try_lock() else {
        return false;
    };
    if guard.len() != src.len() {
        return false;
    }
    guard.copy_from_slice(src);
    true
}

fn read_if_size_matches(mailbox: &Mutex<Vec<f32>>, dst: &mut [f32]) -> bool {
    let Some(guard) = mailbox.try_lock() else {
        return false;
    };
    if guard.len() != dst.len() {
        return false;
    }
    dst.copy_from_slice(&guard);
    true
}

impl AnalyzerGuiShared {
    pub fn new(sample_rate: f32, fft_size: usize) -> Self {
        // +1 for Nyquist — must match `StereoAnalyzer::num_bins()` so the
        // size-matched mailbox publish doesn't silently drop every frame.
        let num_bins = fft_size / 2 + 1;
        Self {
            sample_rate_bits: AtomicU32::new(sample_rate.to_bits()),
            fft_size: AtomicUsize::new(fft_size),
            mid_db: Mutex::new(vec![MIN_DB; num_bins]),
            side_db: Mutex::new(vec![MIN_DB; num_bins]),
            left_db: Mutex::new(vec![MIN_DB; num_bins]),
            right_db: Mutex::new(vec![MIN_DB; num_bins]),
            left_balance_db: Mutex::new(vec![MIN_DB; num_bins]),
            right_balance_db: Mutex::new(vec![MIN_DB; num_bins]),
            correlation_bins: Mutex::new(vec![0.0; num_bins]),
            left_sample_ring: SampleRing::new(SAMPLE_RING_CAPACITY),
            right_sample_ring: SampleRing::new(SAMPLE_RING_CAPACITY),
            spectrogram_source_bits: AtomicU8::new(SpectrogramSource::Mid as u8),
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
            sample_peak_db_bits: AtomicU32::new(MIN_DB.to_bits()),
            sample_peak_max_db_bits: AtomicU32::new(MIN_DB.to_bits()),
            rms_db_bits: AtomicU32::new(MIN_DB.to_bits()),
            rms_max_db_bits: AtomicU32::new(MIN_DB.to_bits()),
            correlation_bits: AtomicU32::new(0.0_f32.to_bits()),
            elapsed_secs_bits: AtomicU32::new(0.0_f32.to_bits()),
            reset_epoch: AtomicU32::new(0),
            ref_analyzing_mask: AtomicU8::new(0),
            right_column_width_bits: AtomicU32::new(RIGHT_COLUMN_WIDTH_DEFAULT.to_bits()),
            top_fraction_bits: AtomicU32::new(TOP_FRACTION_DEFAULT.to_bits()),
        }
    }

    pub fn right_column_width(&self) -> f32 {
        f32::from_bits(self.right_column_width_bits.load(Ordering::Relaxed))
    }

    pub fn set_right_column_width(&self, px: f32) {
        let clean = if px.is_finite() { px.max(0.0) } else { 0.0 };
        self.right_column_width_bits
            .store(clean.to_bits(), Ordering::Relaxed);
    }

    pub fn top_fraction(&self) -> f32 {
        f32::from_bits(self.top_fraction_bits.load(Ordering::Relaxed))
    }

    pub fn set_top_fraction(&self, f: f32) {
        let clean = if f.is_finite() { f.clamp(0.0, 1.0) } else { TOP_FRACTION_DEFAULT };
        self.top_fraction_bits
            .store(clean.to_bits(), Ordering::Relaxed);
    }

    pub fn reset_layout(&self) {
        self.set_right_column_width(RIGHT_COLUMN_WIDTH_DEFAULT);
        self.set_top_fraction(TOP_FRACTION_DEFAULT);
    }

    /// Current spectrogram source, recovered from the `u8` atomic. The
    /// GUI thread reads this once per frame to drive the worker config
    /// and the shader's split-mode flag.
    pub fn spectrogram_source(&self) -> SpectrogramSource {
        SpectrogramSource::from_bits(self.spectrogram_source_bits.load(Ordering::Relaxed))
    }

    /// GUI-thread setter wired to the chip buttons in the spectrogram
    /// toolbar. The change is picked up by the worker on its next
    /// `apply_config` and triggers a history clear so the freshly-
    /// rendered columns don't blend into stale data from the previous
    /// channel.
    pub fn set_spectrogram_source(&self, src: SpectrogramSource) {
        self.spectrogram_source_bits
            .store(src as u8, Ordering::Relaxed);
    }

    /// GUI flips these while an analysis worker is in flight for slot
    /// `slot`. Bits outside `0..REF_SLOT_COUNT` are ignored.
    pub fn set_ref_analyzing(&self, slot: usize, on: bool) {
        if slot >= REF_SLOT_COUNT {
            return;
        }
        let bit = 1u8 << slot;
        if on {
            self.ref_analyzing_mask.fetch_or(bit, Ordering::Relaxed);
        } else {
            self.ref_analyzing_mask.fetch_and(!bit, Ordering::Relaxed);
        }
    }

    pub fn is_ref_analyzing(&self, slot: usize) -> bool {
        if slot >= REF_SLOT_COUNT {
            return false;
        }
        (self.ref_analyzing_mask.load(Ordering::Relaxed) & (1u8 << slot)) != 0
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

    /// Resize all stereo mailboxes to match a new FFT size. Called by
    /// the audio thread after it rebuilds `StereoAnalyzer` on a
    /// user-driven FFT-size change. Updates `fft_size` only after the
    /// mailboxes are grown so GUI readers never observe a size > what
    /// the mailboxes can hold.
    pub fn resize_stereo_mailboxes(&self, fft_size: usize) {
        // +1 for Nyquist — must match `StereoAnalyzer::num_bins()`.
        let num_bins = fft_size / 2 + 1;
        for mailbox in [
            &self.mid_db,
            &self.side_db,
            &self.left_db,
            &self.right_db,
            &self.left_balance_db,
            &self.right_balance_db,
        ] {
            let mut guard = mailbox.lock();
            guard.resize(num_bins, MIN_DB);
        }
        {
            let mut guard = self.correlation_bins.lock();
            guard.resize(num_bins, 0.0);
        }
        self.fft_size.store(fft_size, Ordering::Release);
    }

    /// Audio-thread mailbox write. Returns `false` when the GUI holds
    /// the lock (rare) or when `src.len() != mailbox.len()` (transient
    /// mid-resize state). Callers drop the frame and try again next
    /// hop in either case.
    pub fn try_publish_mid_db(&self, src: &[f32]) -> bool {
        publish_if_size_matches(&self.mid_db, src)
    }

    pub fn try_publish_side_db(&self, src: &[f32]) -> bool {
        publish_if_size_matches(&self.side_db, src)
    }

    /// GUI-thread mailbox read. Returns `false` on lock contention OR
    /// size mismatch (audio thread mid-rebuild). Callers keep their
    /// previous frame's values on miss.
    pub fn try_read_mid_db(&self, dst: &mut [f32]) -> bool {
        read_if_size_matches(&self.mid_db, dst)
    }

    pub fn try_read_side_db(&self, dst: &mut [f32]) -> bool {
        read_if_size_matches(&self.side_db, dst)
    }

    pub fn try_publish_left_db(&self, src: &[f32]) -> bool {
        publish_if_size_matches(&self.left_db, src)
    }

    pub fn try_publish_right_db(&self, src: &[f32]) -> bool {
        publish_if_size_matches(&self.right_db, src)
    }

    pub fn try_read_left_db(&self, dst: &mut [f32]) -> bool {
        read_if_size_matches(&self.left_db, dst)
    }

    pub fn try_read_right_db(&self, dst: &mut [f32]) -> bool {
        read_if_size_matches(&self.right_db, dst)
    }

    pub fn try_publish_left_balance_db(&self, src: &[f32]) -> bool {
        publish_if_size_matches(&self.left_balance_db, src)
    }

    pub fn try_publish_right_balance_db(&self, src: &[f32]) -> bool {
        publish_if_size_matches(&self.right_balance_db, src)
    }

    pub fn try_read_left_balance_db(&self, dst: &mut [f32]) -> bool {
        read_if_size_matches(&self.left_balance_db, dst)
    }

    pub fn try_read_right_balance_db(&self, dst: &mut [f32]) -> bool {
        read_if_size_matches(&self.right_balance_db, dst)
    }

    pub fn try_publish_correlation(&self, src: &[f32]) -> bool {
        publish_if_size_matches(&self.correlation_bins, src)
    }

    pub fn try_read_correlation(&self, dst: &mut [f32]) -> bool {
        read_if_size_matches(&self.correlation_bins, dst)
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
        self.sample_peak_db_bits
            .store(s.sample_peak_db.to_bits(), Ordering::Relaxed);
        self.sample_peak_max_db_bits
            .store(s.sample_peak_max_db.to_bits(), Ordering::Relaxed);
        self.rms_db_bits.store(s.rms_db.to_bits(), Ordering::Relaxed);
        self.rms_max_db_bits
            .store(s.rms_max_db.to_bits(), Ordering::Relaxed);
        self.correlation_bits
            .store(s.correlation.to_bits(), Ordering::Relaxed);
        self.elapsed_secs_bits
            .store(s.elapsed_secs.to_bits(), Ordering::Relaxed);
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
        self.sample_peak_db_bits
            .store(s.sample_peak_db.to_bits(), Ordering::Relaxed);
        self.sample_peak_max_db_bits
            .store(s.sample_peak_max_db.to_bits(), Ordering::Relaxed);
        self.rms_db_bits.store(s.rms_db.to_bits(), Ordering::Relaxed);
        self.rms_max_db_bits
            .store(s.rms_max_db.to_bits(), Ordering::Relaxed);
        self.correlation_bits
            .store(s.correlation.to_bits(), Ordering::Relaxed);
        self.elapsed_secs_bits
            .store(s.elapsed_secs.to_bits(), Ordering::Relaxed);
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
            sample_peak_db: load(&self.sample_peak_db_bits),
            sample_peak_max_db: load(&self.sample_peak_max_db_bits),
            rms_db: load(&self.rms_db_bits),
            rms_max_db: load(&self.rms_max_db_bits),
            correlation: load(&self.correlation_bits),
            elapsed_secs: load(&self.elapsed_secs_bits),
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
    /// Shared GPU device. `Arc` so the CQT worker thread can hold its
    /// own reference and issue its own command buffers against the
    /// same underlying MTLDevice + command queue (both thread-safe
    /// per Apple's Metal docs).
    device: Option<Arc<GpuDevice>>,
    spectrum: Option<SpectrumGpuRenderer>,
    /// CQT worker thread — spawned lazily on first paint so the ~500 ms
    /// kernel construction happens once we know the host sample rate.
    /// Dropped when `EditorState` drops (editor close / plugin destroy),
    /// joining the worker.
    worker: Option<CqtWorker>,
    quad: SharedPainterState,
    mid_scratch: Vec<f32>,
    side_scratch: Vec<f32>,
    left_scratch: Vec<f32>,
    right_scratch: Vec<f32>,
    correlation_scratch: Vec<f32>,
    /// Pre-computed weighting curve uploaded to the shader whenever the
    /// mode or visible freq range changes. Replaces the old per-pixel
    /// biquad sin/cos evaluation — shader now samples this LUT with one
    /// 2-tap blend per pixel.
    weighting_lut_cache: WeightingLutCache,
    /// Cache of Gaussian-smoothed reference envelopes, keyed by
    /// (analysis fingerprint, smoothing mode). Rebuilt only when a slot
    /// gets a new analysis or the user switches smoothing mode — the raw
    /// `smooth_envelope` call is O(REF_POINTS × kernel_radius) with `exp`
    /// and `powf` in the inner loop, so recomputing it 60× per second was
    /// burning serious CPU on the GUI thread.
    ref_envelope_cache: RefEnvelopeCache,
}

#[derive(Default)]
struct RefEnvelopeCache {
    entries: [Option<RefCacheEntry>; REF_SLOT_COUNT],
}

struct RefCacheEntry {
    fingerprint: u64,
    smoothing: FreqSmoothing,
    smoothed: Vec<[f32; 2]>,
}

/// Compact fingerprint of a reference analysis. Changes when a new file
/// is loaded; stable across frames and across visibility toggles. Samples
/// a handful of envelope points rather than hashing all 1024 — the
/// probability that two distinct real audio files produce identical values
/// at every sampled index AND the same integrated LUFS is negligible.
fn ref_analysis_fingerprint(a: &RefAnalysis) -> u64 {
    let bounds = a.mid.bounds.as_slice();
    let n = bounds.len();
    let pick = |i: usize| -> ([f32; 2], u32) {
        let b = bounds.get(i).copied().unwrap_or([MIN_DB, MIN_DB]);
        (b, i as u32)
    };
    let samples = [
        pick(0),
        pick(n / 4),
        pick(n / 2),
        pick((3 * n) / 4),
        pick(n.saturating_sub(1)),
    ];
    let mut h: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
    let mut mix = |x: u32| {
        h ^= x as u64;
        h = h.wrapping_mul(0x100000001b3);
    };
    mix(a.integrated_lufs.to_bits());
    mix(a.lowpass_hz.map(f32::to_bits).unwrap_or(0));
    mix(a.source_sample_rate.to_bits());
    for (pair, idx) in samples {
        mix(pair[0].to_bits());
        mix(pair[1].to_bits());
        mix(idx);
    }
    h
}

struct WeightingLutCache {
    key: Option<(Weighting, f32, f32)>,
    values: Vec<f32>,
}

impl Default for WeightingLutCache {
    fn default() -> Self {
        Self {
            key: None,
            values: vec![0.0; WEIGHTING_LUT_SIZE],
        }
    }
}

/// Populate `out` with the weighting curve sampled on a log-uniform grid
/// over `[freq_min, freq_max]`, with the DC-bias align offset baked in
/// so the shader doesn't need a separate uniform for it.
fn fill_weighting_lut(
    out: &mut [f32],
    weighting: Weighting,
    freq_min: f32,
    freq_max: f32,
) {
    debug_assert_eq!(out.len(), WEIGHTING_LUT_SIZE);
    let align = weighting_align_offset(weighting, freq_min, freq_max);
    if freq_max <= freq_min {
        out.fill(align);
        return;
    }
    let log_min = freq_min.ln();
    let log_max = freq_max.ln();
    let denom = (WEIGHTING_LUT_SIZE - 1) as f32;
    for (i, slot) in out.iter_mut().enumerate() {
        let t = i as f32 / denom;
        let freq = (log_min + t * (log_max - log_min)).exp();
        *slot = weighting_db_at(weighting, freq) + align;
    }
}

pub fn create_editor(
    params: Arc<AnalyzerParams>,
    shared: Arc<AnalyzerGuiShared>,
) -> Option<Box<dyn Editor>> {
    // +1 for Nyquist — must match `StereoAnalyzer::num_bins()`.
    let num_bins = shared.fft_size() / 2 + 1;
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
        left_scratch: vec![MIN_DB; num_bins],
        right_scratch: vec![MIN_DB; num_bins],
        correlation_scratch: vec![0.0; num_bins],
        weighting_lut_cache: WeightingLutCache::default(),
        ref_envelope_cache: RefEnvelopeCache::default(),
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
            // Honour the host window's native DPI so text, lines, and
            // widget edges render at physical-pixel resolution on
            // retina / hi-DPI displays instead of being drawn at 1
            // logical px = 1 physical px and upscaled by the
            // framebuffer (blurry). No-op on 1× displays where
            // native_pixels_per_point == 1.0.
            if let Some(native) = ctx.native_pixels_per_point() {
                if (ctx.pixels_per_point() - native).abs() > 0.01 {
                    ctx.set_pixels_per_point(native);
                }
            }
            // Refs row sits at the very top so the loaded reference
            // tracks read like a "tab strip" above the global controls
            // they affect (overlay + curve match-gain).
            egui::TopBottomPanel::top("analyzer-refs")
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(14, 17, 24)))
                .exact_height(26.0)
                .show(ctx, |ui| {
                    draw_ref_slots(ui, state);
                });
            egui::TopBottomPanel::top("analyzer-controls")
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(18, 22, 30)))
                .exact_height(26.0)
                .show(ctx, |ui| {
                    draw_controls(ui, state, setter);
                });
            // Bottom footer mirrors the top header. Holds the Spec
            // (M / S / L|R + Sharpen + Floor) and L/R range controls
            // so they sit physically next to the visuals they drive
            // (spectrogram + L/R column, both bottom-anchored).
            egui::TopBottomPanel::bottom("analyzer-bottom-controls")
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(18, 22, 30)))
                .exact_height(26.0)
                .show(ctx, |ui| {
                    draw_bottom_controls(ui, state, setter);
                });
            let right_col_w = state.shared.right_column_width();
            egui::SidePanel::right("analyzer-right-column")
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(12, 15, 21)))
                .exact_width(right_col_w)
                .resizable(false)
                .show(ctx, |ui| {
                    draw_right_column(ui, state);
                });
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(8, 10, 14)))
                .show(ctx, |ui| {
                    draw_spectrum(ui, state);
                });
            draw_layout_grab_handle(ctx, state);
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
    // +1 for Nyquist — must match `StereoAnalyzer::num_bins()` and the
    // mailbox sizing in `AnalyzerGuiShared::new`.
    let num_bins = fft_size / 2 + 1;
    if state.mid_scratch.len() != num_bins {
        state.mid_scratch.resize(num_bins, MIN_DB);
    }
    if state.side_scratch.len() != num_bins {
        state.side_scratch.resize(num_bins, MIN_DB);
    }
    if state.left_scratch.len() != num_bins {
        state.left_scratch.resize(num_bins, MIN_DB);
    }
    if state.right_scratch.len() != num_bins {
        state.right_scratch.resize(num_bins, MIN_DB);
    }
    if state.correlation_scratch.len() != num_bins {
        state.correlation_scratch.resize(num_bins, 0.0);
    }

    // Target render-buffer size: rect × DPI, clamped.
    let ppp = ui.ctx().pixels_per_point();
    let available = ui.available_size();
    let phys_w = ((available.x * ppp).round() as u32).clamp(64, MAX_SPECTRUM_W);
    let phys_h = ((available.y * ppp).round() as u32).clamp(32, MAX_SPECTRUM_H);

    // Lazy GPU init.
    if state.device.is_none() {
        state.device = Some(Arc::new(GpuDevice::new()));
    }
    // Spawn the CQT worker the first time we know the sample rate. The
    // worker owns the ~500 ms kernel construction + GPU FFT plan setup;
    // subsequent redraws just drain its output ring.
    if state.worker.is_none() && sr > 0.0 {
        if let Some(device) = state.device.as_ref() {
            let build = spectrum_gpu::cqt_build_params(sr);
            state.worker = Some(CqtWorker::spawn(
                sr,
                state.shared.clone(),
                device.clone(),
                build,
            ));
        }
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
                // Freshly-allocated GPU LUT buffer is zero-initialised
                // (= flat weighting). Invalidate the cache so the next
                // draw re-uploads the current weighting curve; without
                // this, if the user's selection matched the cached key
                // from a previous editor open, the weighting wouldn't
                // re-apply and MS/spectrogram tilt would read as flat.
                state.weighting_lut_cache.key = None;
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
        // LR column reads the symmetrically-smoothed balance magnitudes,
        // not the peak-hold `left_db`/`right_db` the MS curves use —
        // otherwise a past L transient would pin the balance line until
        // a louder R transient came through.
        let _ = state
            .shared
            .try_read_left_balance_db(&mut state.left_scratch);
        let _ = state
            .shared
            .try_read_right_balance_db(&mut state.right_scratch);
        let _ = state
            .shared
            .try_read_correlation(&mut state.correlation_scratch);
        let ss_on = state.params.synchrosqueeze.value();
        let top_fraction = state.shared.top_fraction();
        let weighting = state.params.weighting.value();
        let (freq_min_for_lut, freq_max_for_lut) = {
            let nyquist = sr * 0.5;
            let fmin_u = state.params.freq_min_hz.value() as f32;
            let fmax_u = state.params.freq_max_hz.value() as f32;
            let fmax = fmax_u.min(nyquist).max(fmin_u * 2.0);
            let fmin = fmin_u.min(fmax * 0.5).max(1.0);
            (fmin, fmax)
        };

        // Rebuild the weighting LUT only when mode / range changes (the
        // computation is a pure CPU pass). Uploading to the GPU is
        // cheap (4 KB memcpy into a shared Metal buffer) and defending
        // against edge cases where the buffer might be stale (editor
        // reopen, underlying texture swap) is worth the per-frame cost.
        let lut_key = (weighting, freq_min_for_lut, freq_max_for_lut);
        if state.weighting_lut_cache.key != Some(lut_key) {
            fill_weighting_lut(
                &mut state.weighting_lut_cache.values,
                weighting,
                freq_min_for_lut,
                freq_max_for_lut,
            );
            state.weighting_lut_cache.key = Some(lut_key);
        }
        spec.set_weighting_lut(&state.weighting_lut_cache.values);

        // Resolve sync state for display + worker.
        let (bpm_opt, beat_opt, _playing) = state.shared.transport();
        let sync_requested = state.params.sync.value();
        let beats_per_window = state.params.sync_window.value().beats();
        let sync_active = matches!(
            (sync_requested, bpm_opt, beat_opt),
            (true, Some(bpm), Some(_)) if bpm > 0.0 && beats_per_window > 0.0
        );

        let smoothing = state.params.freq_smoothing.value();
        let (smooth_half, smoothing_mode) = match smoothing.fixed_half_octaves() {
            Some(half) => (half, 0.0_f32),
            None => (0.0, 1.0_f32), // ERB — shader computes per-pixel
        };

        let spectrogram_source = state.shared.spectrogram_source();
        let stacked = matches!(spectrogram_source, SpectrogramSource::LeftRight);

        spec.set_display(DisplayConfig {
            smooth_half_oct_log2: smooth_half,
            smoothing_mode,
            fill_alpha: FILL_ALPHA,
            spectrum_fraction: top_fraction,
            spectrogram_db_min: SPECTROGRAM_DB_MIN,
            spectrogram_db_max: if ss_on { SPECTROGRAM_DB_MAX_SS } else { SPECTROGRAM_DB_MAX_RAW },
            spectrogram_gamma: SPECTROGRAM_GAMMA,
            sync_mode: sync_active,
            stacked_mode: stacked,
        });

        // Hand current transport + display knobs to the worker. It reads
        // this once per hop; the config lock is uncontested here (only
        // the worker touches it otherwise). `source` switches drive a
        // history clear inside the worker so the freshly-rendered
        // columns don't blend with stale Mid frames after a flip to
        // Side or L+R.
        worker.post_config(WorkerConfig {
            sync_enabled: sync_active,
            bpm: bpm_opt.map(|v| v as f32).unwrap_or(0.0),
            beat_pos: beat_opt.unwrap_or(f64::NAN),
            beats_per_window,
            synchrosqueeze: ss_on,
            // Coherence is force-disabled at the boundary. The param
            // is kept (with its default false and full worker code path)
            // for potential future re-exposure, but with no UI to
            // toggle it back off we don't want a previously-saved
            // session resurrecting it. Single-source override here so
            // the param's current value never reaches the worker.
            coherence: false,
            synchro_gate_db: state.params.synchro_gate_db.value(),
            source: spectrogram_source,
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
    let (rect, response) = ui.allocate_exact_size(available, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let nyquist = sr * 0.5;
    let freq_min_user = state.params.freq_min_hz.value() as f32;
    let freq_max_user = state.params.freq_max_hz.value() as f32;
    let freq_max = freq_max_user.min(nyquist).max(freq_min_user * 2.0);
    let freq_min = freq_min_user.min(freq_max * 0.5).max(1.0);
    let top_fraction = state.shared.top_fraction();

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
    // Axis tick labels go pure white so they stand out over the
    // spectrum fills / spectrogram heatmap at a glance.
    let label_color = egui::Color32::WHITE;

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
    // Defensive range clip on grid + label draws: with the current
    // DB_MIN/DB_MAX consts every entry falls inside [-90, 0], but if
    // the range ever becomes configurable we don't want orphaned grid
    // lines or labels floating outside the spectrum panel.
    for &db in DB_MINORS {
        if !(DB_MIN..=DB_MAX).contains(&db) {
            continue;
        }
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
        if !(DB_MIN..=DB_MAX).contains(&db) {
            continue;
        }
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
        draw_outlined_text(
            &painter,
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

    // 2b. Reference-track percentile bands. Drawn after the static
    //     inverse-weighting line so slots with bright fills don't hide
    //     the reference line underneath.
    draw_ref_bands(
        &painter,
        spectrum_rect,
        freq_min,
        freq_max,
        &state.params.ref_slots.read(),
        state.shared.integrated_lufs(),
        state.params.weighting.value(),
        state.params.freq_smoothing.value(),
        &mut state.ref_envelope_cache,
    );

    // 3. Spectrum-region labels. Transparent shader lets these sit on
    //    top of the curves for readability.
    for &freq in FREQ_MAJORS {
        if freq >= freq_min && freq <= freq_max {
            let x = freq_to_x(freq, freq_min, freq_max, spectrum_rect);
            draw_outlined_text(
                &painter,
                egui::pos2(x + 3.0, spectrum_rect.bottom() - 3.0),
                egui::Align2::LEFT_BOTTOM,
                &format_hz(freq),
                egui::FontId::monospace(10.0),
                label_color,
            );
        }
    }
    for &db in DB_MAJORS {
        if !(DB_MIN..=DB_MAX).contains(&db) {
            continue;
        }
        let y = db_to_y(db, DB_MIN, DB_MAX, spectrum_rect);
        draw_outlined_text(
            &painter,
            egui::pos2(spectrum_rect.left() + 3.0, y),
            egui::Align2::LEFT_CENTER,
            &format!("{} dB", db as i32),
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

    // Source / Sharpen / Floor / L/R range live in the global top
    // toolbar (`draw_controls`). Earlier iterations overlaid them on
    // the cells, but the chips covered the spectrogram + L/R column
    // content. Keep the cells unobstructed; the toolbar groups every
    // control by purpose anyway.

    // 5. Crosshair + readout when hovering. Two distinct sub-regions
    //    inside the same ui-allocated rect: the MS plot on top and the
    //    spectrogram below. Spectrum readout shows freq + cursor-dB +
    //    actual Mid/Side dB at that freq; spectrogram shows freq +
    //    beat (sync mode) + channel (stacked L+R mode).
    if let Some(cursor) = response.hover_pos() {
        let weighting = state.params.weighting.value();
        let weighting_align = weighting_align_offset(weighting, freq_min, freq_max);
        let fft_size = state.shared.fft_size();
        if spectrum_rect.contains(cursor) {
            let freq = x_to_freq(cursor.x, freq_min, freq_max, spectrum_rect);
            let cursor_db = y_to_db(cursor.y, DB_MIN, DB_MAX, spectrum_rect);
            let w_db = weighting_db_at(weighting, freq) + weighting_align;
            let mid_raw = sample_bin_db(&state.mid_scratch, freq, sr, fft_size);
            let side_raw = sample_bin_db(&state.side_scratch, freq, sr, fft_size);
            let mid_db = mid_raw + w_db;
            let side_db = side_raw + w_db;
            let lines = vec![
                format_hz_readout(freq),
                format!("y: {:>6.1} dB", cursor_db),
                format!("M: {:>6.1} dB", mid_db.max(MIN_DB)),
                format!("S: {:>6.1} dB", side_db.max(MIN_DB)),
            ];
            let p = painter.with_clip_rect(spectrum_rect);
            draw_crosshair(&p, spectrum_rect, cursor, &lines);
        } else if spectrogram_rect.contains(cursor) && spectrogram_rect.height() > 4.0 {
            let stacked = matches!(
                state.shared.spectrogram_source(),
                SpectrogramSource::LeftRight
            );
            let mut lines: Vec<String> = Vec::with_capacity(5);
            let freq;
            // `freq_rect` is the sub-rect that the cursor's y maps into
            // (top half, bottom half, or full). We use it both to recover
            // the freq and to compute the column from x — the shader's
            // x-axis math is on the full spectrogram width, so for the
            // column lookup we always use spectrogram_rect, not the
            // sub-rect.
            let mut buffer_id: u32 = 0;
            if stacked {
                let mid_y = spectrogram_rect.center().y;
                if cursor.y < mid_y {
                    let top = egui::Rect::from_min_max(
                        spectrogram_rect.min,
                        egui::pos2(spectrogram_rect.right(), mid_y),
                    );
                    freq = y_to_freq_log(cursor.y, freq_min, freq_max, top);
                    lines.push("ch: L".to_string());
                    buffer_id = 0;
                } else {
                    let bot = egui::Rect::from_min_max(
                        egui::pos2(spectrogram_rect.left(), mid_y),
                        spectrogram_rect.max,
                    );
                    freq = y_to_freq_log(cursor.y, freq_min, freq_max, bot);
                    lines.push("ch: R".to_string());
                    buffer_id = 1;
                }
            } else {
                let label = match state.shared.spectrogram_source() {
                    SpectrogramSource::Mid => "ch: M",
                    SpectrogramSource::Side => "ch: S",
                    SpectrogramSource::LeftRight => "ch: L|R",
                };
                lines.push(label.to_string());
                freq = y_to_freq_log(cursor.y, freq_min, freq_max, spectrogram_rect);
            }
            lines.push(format_hz_readout(freq));

            // Sync state still affects the column lookup below (it picks
            // the spread-across-width math vs the 1-pixel-per-column
            // free-scroll math), but the bar.beat readout itself is not
            // shown — frequency + dB are what the user actually wants
            // when probing the heatmap.
            let (bpm_opt, beat_opt, _) = state.shared.transport();
            let sync_on = state.params.sync.value();
            let beats_per_window = state.params.sync_window.value().beats();
            let sync_active = sync_on
                && bpm_opt.map(|b| b > 0.0).unwrap_or(false)
                && beat_opt.is_some()
                && beats_per_window > 0.0;

            // Spectrogram dB lookup. Mirrors the shader's column math:
            // sync mode spreads HISTORY_COLS columns across rect width;
            // free-scroll uses 1 texture pixel = 1 column with the right
            // edge anchored at write_col. Reads the CPU-mappable Metal
            // shared buffer directly — no GPU work, no cost beyond a
            // single bin-pair interp.
            if let Some(spec) = state.spectrum.as_ref() {
                let log_bin_f = (CQT_BINS_PER_OCTAVE as f32) * (freq / CQT_FMIN_HZ).log2();
                let history_cols_i = HISTORY_COLS as i32;
                let col = if sync_active {
                    let frac = ((cursor.x - spectrogram_rect.left())
                        / spectrogram_rect.width().max(1.0))
                        .clamp(0.0, 0.9999);
                    (frac * HISTORY_COLS as f32) as u32
                } else {
                    let ppp = ui.ctx().pixels_per_point();
                    let texture_w = spec.width() as i32;
                    let px_col = ((cursor.x - spectrogram_rect.left()) * ppp).floor() as i32;
                    let history_idx = ((texture_w - 1) - px_col).max(0).min(history_cols_i - 1);
                    let mut c = spec.write_col() as i32 - history_idx;
                    c = ((c % history_cols_i) + history_cols_i) % history_cols_i;
                    c as u32
                };
                if let Some(raw_db) = spec.sample_history_db(buffer_id, col, log_bin_f) {
                    // Floor reads on freshly-cleared columns come back at
                    // the silence sentinel (~-140 dB). Don't bother
                    // showing a "level" for those — display "—" instead
                    // so the user can tell empty cells from real signal.
                    if raw_db > -130.0 {
                        let weighted = raw_db + weighting_db_at(weighting, freq);
                        lines.push(format!("z: {:>6.1} dB", weighted));
                    } else {
                        lines.push("z:     —".to_string());
                    }
                }
            }

            let p = painter.with_clip_rect(spectrogram_rect);
            draw_crosshair(&p, spectrogram_rect, cursor, &lines);
        }
    }
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
    // Shape of the adjustment applied by the analyser, pinned so the
    // peak touches 0 dB (= the freq the weighting boosts MOST). The
    // shader's actual adjustment is mean-zero thanks to the align
    // offset, but for modes like Pink that span ±20 dB the positive
    // half would clip off the top of the MS plot — pinning to the
    // peak keeps the shape faithful without losing the upper half.
    //
    // Reading:
    //   curve near 0 dB   → this bin gets the MOST boost from the
    //                       weighting (loudness-efficient: content
    //                       here drives LUFS hard)
    //   curve further down → this bin is weighted down relative to
    //                        the peak (loudness-inefficient: eats
    //                        peak budget without helping loudness)
    let stats = weighting_stats(weighting, freq_min, freq_max);
    let n = 128usize;
    let log_min = freq_min.ln();
    let log_max = freq_max.ln();
    let mut pts = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let freq = (log_min + t * (log_max - log_min)).exp();
        let adj_db = weighting_db_at(weighting, freq) - stats.max;
        let x = rect.left() + t * rect.width();
        let y = db_to_y(adj_db, DB_MIN, DB_MAX, rect);
        pts.push(egui::pos2(x, y));
    }
    painter.add(egui::Shape::line(
        pts,
        egui::Stroke::new(1.5, egui::Color32::from_white_alpha(180)),
    ));
}

/// Draw the loaded reference slots as filled percentile bands on the
/// MS plot. Each visible slot contributes one band; bands are gain-
/// matched to the live mix's integrated LUFS so *shape* comparison is
/// honest regardless of relative level. MP3 codec lowpass (if detected)
/// clips the band at the brickwall so the display doesn't misleadingly
/// show the ref "falling off" above the codec cutoff.
fn draw_ref_bands(
    painter: &egui::Painter,
    rect: egui::Rect,
    freq_min: f32,
    freq_max: f32,
    slots: &RefSlots,
    live_integrated_lufs: f32,
    weighting: Weighting,
    smoothing_mode: FreqSmoothing,
    cache: &mut RefEnvelopeCache,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let log_min = REF_FREQ_MIN.ln();
    let log_max = REF_FREQ_MAX.ln();
    let grid_span = log_max - log_min;
    if !grid_span.is_finite() || grid_span <= 0.0 {
        return;
    }

    // Match the live curves' weighting transform so the ref reshapes
    // along with them when the user toggles Pink / Tilted / LUFS. Align
    // offset cancels the curve's DC bias over the visible range — same
    // normalisation the shader's weighting LUT applies to Mid/Side.
    let weight_align = weighting_align_offset(weighting, freq_min, freq_max);

    for (slot_idx, slot) in slots.slots.iter().enumerate() {
        if !slot.visible {
            continue;
        }
        let Some(analysis) = &slot.analysis else {
            continue;
        };
        let envelope = &analysis.mid;
        if envelope.bounds.len() != REF_POINTS {
            continue;
        }

        // Match-to-current gain. Only shift when both the ref and the
        // live mix have a sensible integrated value; otherwise display
        // at source level so a fresh / silent host doesn't drag the
        // ref off-screen.
        let shift_db = if live_integrated_lufs > MIN_DB + 1.0
            && analysis.integrated_lufs > MIN_DB + 1.0
        {
            live_integrated_lufs - analysis.integrated_lufs
        } else {
            0.0
        };

        // Upper cutoff: the band is meaningful only up to min(source
        // Nyquist, LAME lowpass if set). Beyond that the percentile
        // is computed from codec-filtered silence and would misread.
        let source_nyquist = analysis.source_sample_rate * 0.5;
        let upper_cutoff_hz = analysis
            .lowpass_hz
            .map(|lp| lp.min(source_nyquist))
            .unwrap_or(source_nyquist);

        let c = REF_SLOT_COLORS[slot_idx];
        let line_color = egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], 220);

        // Gaussian-smoothed percentile curves. Width follows the
        // toolbar smoothing mode; ERB mode varies σ per grid point to
        // match the ear's critical-band curve (wide low-end smoothing,
        // tight above 5 kHz). Cached per slot — the result only changes
        // when the user loads a new file or switches smoothing mode, so
        // reusing it saves ~60–120 K exp/powf calls per frame per slot.
        let fp = ref_analysis_fingerprint(analysis);
        let need_rebuild = match &cache.entries[slot_idx] {
            Some(e) => e.fingerprint != fp || e.smoothing != smoothing_mode,
            None => true,
        };
        if need_rebuild {
            let smoothed = smooth_envelope(&envelope.bounds, smoothing_mode);
            cache.entries[slot_idx] = Some(RefCacheEntry {
                fingerprint: fp,
                smoothing: smoothing_mode,
                smoothed,
            });
        }
        let smoothed = &cache.entries[slot_idx]
            .as_ref()
            .expect("cache entry populated above")
            .smoothed;

        // Sub-sample the smoothed envelope with a Catmull-Rom cubic
        // spline between adjacent grid points. 4× linear upsample left
        // visible knot kinks at heavy zoom + wide smoothing — the
        // segments between two smoothed values read as horizontal
        // stairs when both values are close, producing a stepped look
        // on what should be a smooth curve. Catmull-Rom uses four
        // neighbours to compute each render point with C1 continuity,
        // indistinguishable from the smooth curves commercial
        // analysers draw.
        //
        // Break the line into disjoint segments wherever we hit an
        // invalid render point (off-screen, past the codec cutoff, or
        // either flanking source grid value sitting at the silence
        // floor). Otherwise the single-Shape::line approach draws a
        // straight line across the gap, producing misleading descents
        // into noise-floor regions at the low end when Smooth=None.
        const REF_RENDER_UPSAMPLE: usize = 8;
        // Catmull-Rom cubic interpolation at fractional position t in
        // [0,1] between p1 and p2, using neighbours p0 and p3.
        fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
            let t2 = t * t;
            let t3 = t2 * t;
            0.5 * ((2.0 * p1)
                + (-p0 + p2) * t
                + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
                + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
        }
        let render_points = REF_POINTS * REF_RENDER_UPSAMPLE;
        let render_denom = (render_points - 1) as f32;
        let src_denom = (REF_POINTS - 1) as f32;
        let floor_threshold = MIN_DB + 1.0;
        let mut upper_segments: Vec<Vec<egui::Pos2>> = Vec::new();
        let mut lower_segments: Vec<Vec<egui::Pos2>> = Vec::new();
        let mut cur_upper: Vec<egui::Pos2> = Vec::new();
        let mut cur_lower: Vec<egui::Pos2> = Vec::new();
        let flush_one = |cur: &mut Vec<egui::Pos2>,
                         segs: &mut Vec<Vec<egui::Pos2>>| {
            if cur.len() >= 2 {
                segs.push(std::mem::take(cur));
            } else {
                cur.clear();
            }
        };
        for render_i in 0..render_points {
            let t = render_i as f32 / render_denom;
            let freq = (log_min + t * grid_span).exp();
            if freq < freq_min || freq > freq_max || freq > upper_cutoff_hz {
                flush_one(&mut cur_upper, &mut upper_segments);
                flush_one(&mut cur_lower, &mut lower_segments);
                continue;
            }
            let src_f = t * src_denom;
            let src_1 = (src_f.floor() as usize).min(REF_POINTS - 1);
            let src_2 = (src_1 + 1).min(REF_POINTS - 1);
            let src_0 = src_1.saturating_sub(1);
            let src_3 = (src_1 + 2).min(REF_POINTS - 1);
            let frac = src_f - src_1 as f32;
            let pair_0 = smoothed[src_0];
            let pair_1 = smoothed[src_1];
            let pair_2 = smoothed[src_2];
            let pair_3 = smoothed[src_3];
            // Per-curve validity: check the two nearest knots (the
            // bracketing source points). If either is at floor, the
            // interp segment is inside a silent region — break it.
            // Outer knots (0, 3) only affect the spline's shape, not
            // its validity, so we don't gate on them.
            let upper_valid = pair_1[1] > floor_threshold && pair_2[1] > floor_threshold;
            let lower_valid = pair_1[0] > floor_threshold && pair_2[0] > floor_threshold;
            let weight_db = weighting_db_at(weighting, freq) + weight_align;
            let x = freq_to_x(freq, freq_min, freq_max, rect);
            if upper_valid {
                let hi_val = catmull_rom(pair_0[1], pair_1[1], pair_2[1], pair_3[1], frac)
                    + shift_db
                    + weight_db;
                cur_upper.push(egui::pos2(x, db_to_y(hi_val, DB_MIN, DB_MAX, rect)));
            } else {
                flush_one(&mut cur_upper, &mut upper_segments);
            }
            if lower_valid {
                let lo_val = catmull_rom(pair_0[0], pair_1[0], pair_2[0], pair_3[0], frac)
                    + shift_db
                    + weight_db;
                cur_lower.push(egui::pos2(x, db_to_y(lo_val, DB_MIN, DB_MAX, rect)));
            } else {
                flush_one(&mut cur_lower, &mut lower_segments);
            }
        }
        flush_one(&mut cur_upper, &mut upper_segments);
        flush_one(&mut cur_lower, &mut lower_segments);
        if upper_segments.is_empty() && lower_segments.is_empty() {
            continue;
        }

        // Two unfilled outlines — upper (90th percentile) and lower
        // (10th). Each contiguous valid region gets its own
        // Shape::line so silence gaps show as gaps, not skated-over
        // straight lines. Upper and lower break independently so a
        // sparse low-end where p10 bottoms out doesn't hide p90's
        // real content.
        let stroke = egui::Stroke::new(1.25, line_color);
        for seg in upper_segments {
            painter.add(egui::Shape::line(seg, stroke));
        }
        for seg in lower_segments {
            painter.add(egui::Shape::line(seg, stroke));
        }
    }
}

/// Gaussian-smooth both percentile curves of a reference envelope,
/// skipping `MIN_DB` slots so out-of-range / silent bins don't drag
/// neighbouring samples down. Fixed modes use one σ across the grid.
/// ERB mode computes σ per-point from Moore & Glasberg's critical-band
/// curve (grid points per octave × ERB half-width at that grid point's
/// frequency) — wide at the low end, tight at the high end, matching
/// the main MS curves' shader behaviour in ERB mode.
fn smooth_envelope(bounds: &[[f32; 2]], smoothing: FreqSmoothing) -> Vec<[f32; 2]> {
    let n = bounds.len();
    let mut out = vec![[MIN_DB, MIN_DB]; n];
    if n == 0 {
        return out;
    }
    if matches!(smoothing, FreqSmoothing::None) {
        out.copy_from_slice(bounds);
        return out;
    }

    let log_min = REF_FREQ_MIN.ln();
    let log_max = REF_FREQ_MAX.ln();
    let log_span = (log_max - log_min).max(1e-6);
    let points_per_octave = (n.saturating_sub(1)) as f32
        / (log_span / std::f32::consts::LN_2);

    let fixed_sigma = ref_smooth_sigma_points(smoothing);

    let threshold = MIN_DB + 1.0;
    for (i, slot) in out.iter_mut().enumerate() {
        // Per-point σ. Fixed modes use one value; ERB scales σ with the
        // ERB half-width at this grid point's frequency.
        let sigma = match fixed_sigma {
            Some(s) => s.max(0.5),
            None => {
                let t = i as f32 / (n - 1) as f32;
                let freq = (log_min + t * log_span).exp();
                (erb_half_octaves_at(freq) * points_per_octave).max(0.5)
            }
        };
        let radius = (sigma * REF_SMOOTH_RADIUS_SIGMAS).ceil() as usize;
        let radius = radius.min(n.saturating_sub(1));
        let lo_i = i.saturating_sub(radius);
        let hi_i = (i + radius + 1).min(n);
        let inv_sigma_sq = 1.0 / (sigma * sigma);
        let mut lo_pow_sum = 0.0f32;
        let mut lo_wsum = 0.0f32;
        let mut hi_pow_sum = 0.0f32;
        let mut hi_wsum = 0.0f32;
        for (offset, pair) in bounds[lo_i..hi_i].iter().enumerate() {
            let j = lo_i + offset;
            let d = j as f32 - i as f32;
            let w = (-0.5 * d * d * inv_sigma_sq).exp();
            if pair[0] > threshold {
                lo_pow_sum += 10.0_f32.powf(pair[0] * 0.1) * w;
                lo_wsum += w;
            }
            if pair[1] > threshold {
                hi_pow_sum += 10.0_f32.powf(pair[1] * 0.1) * w;
                hi_wsum += w;
            }
        }
        *slot = [
            if lo_wsum > 0.0 {
                10.0 * (lo_pow_sum / lo_wsum).max(1e-30).log10()
            } else {
                MIN_DB
            },
            if hi_wsum > 0.0 {
                10.0 * (hi_pow_sum / hi_wsum).max(1e-30).log10()
            } else {
                MIN_DB
            },
        ];
    }
    out
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
    let label_color = egui::Color32::WHITE;
    let stacked = matches!(
        state.shared.spectrogram_source(),
        SpectrogramSource::LeftRight
    );

    // L+R stacked mode: each half remaps the full freq range, so draw
    // grid + labels into both sub-regions and a thin divider line
    // between them. Otherwise draw a single full-height grid.
    if stacked {
        let mid_y = rect.center().y;
        let top_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), mid_y));
        let bot_rect = egui::Rect::from_min_max(egui::pos2(rect.left(), mid_y), rect.max);
        for sub_rect in [top_rect, bot_rect] {
            for &freq in FREQ_MAJORS {
                if freq < freq_min || freq > freq_max {
                    continue;
                }
                let t = (freq / freq_min).ln() / (freq_max / freq_min).ln();
                let y = sub_rect.bottom() - t.clamp(0.0, 1.0) * sub_rect.height();
                painter.line_segment(
                    [egui::pos2(sub_rect.left(), y), egui::pos2(sub_rect.right(), y)],
                    (1.0, grid_over),
                );
                draw_outlined_text(
                    painter,
                    egui::pos2(sub_rect.right() - 4.0, y - 1.0),
                    egui::Align2::RIGHT_BOTTOM,
                    &format_hz(freq),
                    egui::FontId::monospace(10.0),
                    label_color,
                );
            }
        }
        // Channel labels — small badge at the bottom-left of each half
        // so the user knows which channel is which without referring
        // back to the chip toolbar at the top.
        for (sub_rect, tag) in [(top_rect, "L"), (bot_rect, "R")] {
            draw_outlined_text(
                painter,
                egui::pos2(sub_rect.left() + 4.0, sub_rect.bottom() - 2.0),
                egui::Align2::LEFT_BOTTOM,
                tag,
                egui::FontId::monospace(11.0),
                label_color,
            );
        }
        // Hairline between top (L) and bottom (R). 1.5 px so it stays
        // visible against bright spectrogram content.
        painter.line_segment(
            [egui::pos2(rect.left(), mid_y), egui::pos2(rect.right(), mid_y)],
            (1.5, egui::Color32::from_white_alpha(140)),
        );
    } else {
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
            draw_outlined_text(
                painter,
                egui::pos2(rect.right() - 4.0, y - 1.0),
                egui::Align2::RIGHT_BOTTOM,
                &format_hz(freq),
                egui::FontId::monospace(10.0),
                label_color,
            );
        }
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
    let beats_per_window = state.params.sync_window.value().beats();
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
        // Beat/bar label anchored to the *bottom* of the spectrogram
        // so it doesn't visually collide with the MS plot's bottom
        // edge / freq axis sitting immediately above.
        let label_y = rect.bottom() - 2.0;
        if step_beats <= 2.0 {
            // Beat number inside the current bar, 1-indexed.
            let beat_in_bar = beat_idx_in_bar.rem_euclid(BEATS_PER_BAR as i64) + 1;
            draw_outlined_text(
                painter,
                egui::pos2(x + 3.0, label_y),
                egui::Align2::LEFT_BOTTOM,
                &format!("{}", beat_in_bar),
                egui::FontId::monospace(10.0),
                label_color,
            );
        } else {
            // Multi-bar step: show bar number instead.
            let bar_num = beat_idx_in_bar.div_euclid(BEATS_PER_BAR as i64) + 1;
            draw_outlined_text(
                painter,
                egui::pos2(x + 3.0, label_y),
                egui::Align2::LEFT_BOTTOM,
                &format!("{}.1", bar_num),
                egui::FontId::monospace(10.0),
                label_color,
            );
        }
        i += step_beats;
    }
}

/// Draw text with a 1-pixel black outline in all 8 compass directions.
/// Makes small labels readable on the noisy spectrum / spectrogram /
/// heatmap backgrounds without having to fade the background or
/// recolour the text. egui caches galleys by (text, font, color) so
/// the 9 draws reuse the same shaped glyphs under the hood.
fn draw_outlined_text(
    painter: &egui::Painter,
    pos: egui::Pos2,
    anchor: egui::Align2,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
) {
    const OUTLINE_OFFSETS: [(f32, f32); 8] = [
        (-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0),
        (-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0),
    ];
    let outline = egui::Color32::BLACK;
    for (dx, dy) in OUTLINE_OFFSETS {
        painter.text(
            pos + egui::vec2(dx, dy),
            anchor,
            text,
            font.clone(),
            outline,
        );
    }
    painter.text(pos, anchor, text, font, color);
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

/// L/R balance column. Frequency on the y-axis (log, bottom = low,
/// top = high); horizontal axis shows which channel dominates at each
/// bin via a single `L_dB − R_dB` line. Centerline = balanced, left
/// of centerline = L louder, right = R louder. Weighting is applied
/// to both channels before the subtraction so the balance reads in
/// the same perceptual space as the MS plot / spectrogram.
fn draw_lr_column(ui: &mut egui::Ui, state: &mut EditorState) {
    // Half-range of the imbalance axis in dB. The axis spans
    // `[−range_db, +range_db]`; the toolbar `L/R dB` control drives
    // this. Anything beyond the range clamps to the column edge.
    let range_db = state.params.lr_range_db.value().max(1) as f32;
    /// Louder channel must be at or above this level for the balance
    /// line to register as "real". Below it, the line eases back to
    /// centre over `LR_SILENCE_DB` so quiet bins don't randomly bounce.
    const LR_GATE_DB: f32 = -70.0;
    /// Full silence floor for the gate fade.
    const LR_SILENCE_DB: f32 = -95.0;
    // Smoothing is driven by the toolbar dropdown so the L/R column
    // tracks the MS plot above it through mode changes. ERB returns
    // `None` here — we compute the half-width per-row inside the loop.
    let smoothing = state.params.freq_smoothing.value();
    let fixed_half = smoothing.fixed_half_octaves();

    let sr = state.shared.sample_rate();
    let available = ui.available_size();
    if available.x <= 1.0 || available.y <= 1.0 || sr <= 0.0 {
        let (_rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());
        return;
    }
    let (rect, response) = ui.allocate_exact_size(available, egui::Sense::hover());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(8, 10, 14));

    let nyquist = sr * 0.5;
    let freq_min_user = state.params.freq_min_hz.value() as f32;
    let freq_max_user = state.params.freq_max_hz.value() as f32;
    let freq_max = freq_max_user.min(nyquist).max(freq_min_user * 2.0);
    let freq_min = freq_min_user.min(freq_max * 0.5).max(1.0);
    let log_min = freq_min.ln();
    let log_max = freq_max.ln();
    let log_span = (log_max - log_min).max(1e-6);

    // Match the MS plot / spectrogram's perceptual correction: add the
    // same Weighting curve to every per-bin dB value. Without this the
    // L/R column would read raw FFT levels while the chart above shows
    // LUFS-weighted levels, and the eye can't line them up.
    let weighting = state.params.weighting.value();
    let weighting_align = weighting_align_offset(weighting, freq_min, freq_max);

    let freq_to_y = |freq: f32| -> f32 {
        let t = (freq.ln() - log_min) / log_span;
        rect.bottom() - t * rect.height()
    };

    let center_x = rect.center().x;
    // Each half of the column spans `range_db` dB of imbalance.
    let px_per_db = (rect.width() * 0.5) / range_db;

    // Grid drawn after the correlation heatmap so lines remain
    // visible over the colours. Alpha'd down so the colours dominate.
    let grid_over_color = egui::Color32::from_rgba_unmultiplied(18, 22, 28, 160);
    let label_color = egui::Color32::WHITE;

    let fft_size = state.shared.fft_size();
    let num_bins = state
        .left_scratch
        .len()
        .min(state.right_scratch.len())
        .min(state.correlation_scratch.len());
    if num_bins < 2 {
        return;
    }
    let bin_per_hz = fft_size as f32 / sr;
    let max_bin = (num_bins - 1) as f32;

    // Precompute Blackman-Harris weights once — same weights used for
    // every row, same shape as the MS shader's tapered window so the
    // LR column's peaks/valleys read the same way vs the curves above
    // (and no rectangular plateaus around narrow peaks).
    const LR_SMOOTH_N: usize = 32;
    let mut bh_weights = [0.0f32; LR_SMOOTH_N];
    for (i, w) in bh_weights.iter_mut().enumerate() {
        let phase = (i as f32 + 0.5) * (std::f32::consts::TAU / LR_SMOOTH_N as f32);
        *w = 0.35875 - 0.48829 * phase.cos() + 0.14128 * (2.0 * phase).cos()
            - 0.01168 * (3.0 * phase).cos();
    }

    // Walk one point per pixel row. Recomputed every frame so the UI
    // follows the audio publish cadence at display refresh rate — the
    // smoothed values do change once per audio hop, and tying the
    // recompute to a cache would introduce visible stepping between
    // publishes. Packs the balance-line vertices and correlation-strip
    // mesh in a single pass.
    let rows = rect.height().ceil() as i32 + 1;
    let mut pts = Vec::with_capacity(rows as usize);
    let mut strip_mesh = egui::epaint::Mesh::default();
    strip_mesh.vertices.reserve((rows as usize) * 2);
    strip_mesh
        .indices
        .reserve((rows as usize).saturating_sub(1) * 6);

    for row in 0..rows {
        let y = rect.bottom() - row as f32;
        if y < rect.top() {
            break;
        }
        let t = (rect.bottom() - y) / rect.height().max(1.0);
        let freq = (log_min + t * log_span).exp();
        let half_oct = fixed_half.unwrap_or_else(|| erb_half_octaves_at(freq));
        let (l_db_raw, r_db_raw, corr) = if half_oct <= 0.0 {
            // No smoothing — single-bin linear interp lookup.
            let bin_f = (freq * bin_per_hz).clamp(0.0, max_bin);
            let b0 = bin_f.floor() as usize;
            let b1 = (b0 + 1).min(num_bins - 1);
            let frac = bin_f - b0 as f32;
            let omf = 1.0 - frac;
            let l_pow = 10.0_f32.powf(state.left_scratch[b0] * 0.1) * omf
                + 10.0_f32.powf(state.left_scratch[b1] * 0.1) * frac;
            let r_pow = 10.0_f32.powf(state.right_scratch[b0] * 0.1) * omf
                + 10.0_f32.powf(state.right_scratch[b1] * 0.1) * frac;
            (
                10.0 * l_pow.max(1e-30).log10(),
                10.0 * r_pow.max(1e-30).log10(),
                state.correlation_scratch[b0] * omf + state.correlation_scratch[b1] * frac,
            )
        } else {
            // BH-tapered weighted average across the window. Matches
            // the MS shader's window shape so peaks read as bells
            // here too instead of flat-topped plateaus.
            let bin_lo_f = (freq * (-half_oct).exp2() * bin_per_hz).clamp(0.0, max_bin);
            let bin_hi_f = (freq * half_oct.exp2() * bin_per_hz).clamp(0.0, max_bin);
            let span_bins = (bin_hi_f - bin_lo_f).max(1e-6);
            let step = span_bins / LR_SMOOTH_N as f32;
            let mut l_pow_sum = 0.0f32;
            let mut r_pow_sum = 0.0f32;
            let mut c_sum = 0.0f32;
            let mut w_sum = 0.0f32;
            for (i, &w) in bh_weights.iter().enumerate() {
                let b_f = (bin_lo_f + (i as f32 + 0.5) * step).clamp(0.0, max_bin);
                let b0 = b_f.floor() as usize;
                let b1 = (b0 + 1).min(num_bins - 1);
                let frac = b_f - b0 as f32;
                let omf = 1.0 - frac;
                let l_db_samp = state.left_scratch[b0] * omf + state.left_scratch[b1] * frac;
                let r_db_samp = state.right_scratch[b0] * omf + state.right_scratch[b1] * frac;
                let c_samp = state.correlation_scratch[b0] * omf
                    + state.correlation_scratch[b1] * frac;
                l_pow_sum += 10.0_f32.powf(l_db_samp * 0.1) * w;
                r_pow_sum += 10.0_f32.powf(r_db_samp * 0.1) * w;
                c_sum += c_samp * w;
                w_sum += w;
            }
            (
                10.0 * (l_pow_sum / w_sum).max(1e-30).log10(),
                10.0 * (r_pow_sum / w_sum).max(1e-30).log10(),
                c_sum / w_sum,
            )
        };
        let weighting_db = weighting_db_at(weighting, freq) + weighting_align;
        let l_db = l_db_raw + weighting_db;
        let r_db = r_db_raw + weighting_db;

        // Soft silence gate: multiply the raw delta by a 0..1 fade
        // based on the louder channel's level. Below `LR_SILENCE_DB`
        // the line stays dead-centre and the correlation strip
        // desaturates to a neutral dark so noise-floor bins don't
        // flash red/green. No clamp on the delta itself — overshoots
        // past ±range just hit the painter's clip rect and visually
        // "run off" the edge.
        let louder = l_db.max(r_db);
        let fade = ((louder - LR_SILENCE_DB) / (LR_GATE_DB - LR_SILENCE_DB)).clamp(0.0, 1.0);
        let delta_db = (l_db - r_db) * fade;
        let x = center_x - delta_db * px_per_db;
        pts.push(egui::pos2(x, y));

        // Correlation colour for this row, faded to black in silent bins
        // so the heatmap only lights up where there's real signal. The
        // strip used to be ~450 individual `rect_filled` calls per frame
        // — a real hot spot in egui's tessellator. A single mesh with
        // two verts per row and two triangles bridging adjacent rows
        // produces identical output for one shape instead of hundreds.
        let corr_color = correlation_color_ramp(corr);
        let color = lerp_color_rgb(egui::Color32::BLACK, corr_color, fade);
        let vi = strip_mesh.vertices.len() as u32;
        strip_mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(rect.left(), y),
            uv: egui::epaint::WHITE_UV,
            color,
        });
        strip_mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(rect.right(), y),
            uv: egui::epaint::WHITE_UV,
            color,
        });
        if vi >= 2 {
            strip_mesh.indices.extend_from_slice(&[
                vi - 2, vi - 1, vi,
                vi - 1, vi + 1, vi,
            ]);
        }
    }
    if !strip_mesh.indices.is_empty() {
        painter.add(egui::Shape::mesh(strip_mesh));
    }

    // Grid on top of the heatmap. Slightly alpha'd so colours read
    // through; major frequency ticks + dB tick lines keep the axis
    // readable without fighting the heatmap.
    for &freq in FREQ_MAJORS {
        if freq >= freq_min && freq <= freq_max {
            let y = freq_to_y(freq);
            painter.line_segment(
                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                (1.0, grid_over_color),
            );
        }
    }
    let step_db = if range_db >= 20.0 { 10.0_f32 } else { 5.0_f32 };
    let mut db_off = step_db;
    while db_off < range_db - 0.5 {
        let d = db_off * px_per_db;
        for x in [center_x - d, center_x + d] {
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                (1.0, grid_over_color),
            );
        }
        db_off += step_db;
    }
    // Centerline: thicker + high-contrast so "balanced" reads at a
    // glance against the colourful correlation heatmap behind.
    let centerline_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 220);
    painter.line_segment(
        [
            egui::pos2(center_x, rect.top()),
            egui::pos2(center_x, rect.bottom()),
        ],
        (2.0, centerline_color),
    );

    // Balance curve in white — reads cleanly over the heatmap and
    // doesn't compete with Mid/Side/ref colours.
    let line_color = egui::Color32::from_rgb(240, 245, 250);
    if pts.len() >= 2 {
        painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5, line_color)));
    }

    // L / R side hints in the top corners so the user knows which
    // direction means what without hovering for a tooltip.
    let side_hint_color = egui::Color32::WHITE;
    draw_outlined_text(
        &painter,
        egui::pos2(rect.left() + 4.0, rect.top() + 3.0),
        egui::Align2::LEFT_TOP,
        "L",
        egui::FontId::monospace(10.0),
        side_hint_color,
    );
    draw_outlined_text(
        &painter,
        egui::pos2(rect.right() - 4.0, rect.top() + 3.0),
        egui::Align2::RIGHT_TOP,
        "R",
        egui::FontId::monospace(10.0),
        side_hint_color,
    );
    // Axis ticks: outer edges = `range` dB of imbalance toward that
    // side; centre = balanced (Δ = 0).
    draw_outlined_text(
        &painter,
        egui::pos2(rect.left() + 2.0, rect.bottom() - 2.0),
        egui::Align2::LEFT_BOTTOM,
        &format!("{}", range_db as i32),
        egui::FontId::monospace(9.0),
        label_color,
    );
    draw_outlined_text(
        &painter,
        egui::pos2(center_x, rect.bottom() - 2.0),
        egui::Align2::CENTER_BOTTOM,
        "0 dB",
        egui::FontId::monospace(9.0),
        label_color,
    );
    draw_outlined_text(
        &painter,
        egui::pos2(rect.right() - 2.0, rect.bottom() - 2.0),
        egui::Align2::RIGHT_BOTTOM,
        &format!("{}", range_db as i32),
        egui::FontId::monospace(9.0),
        label_color,
    );

    // L/R range lives in the global top toolbar (`draw_controls`)
    // alongside the spectrogram source / Sharpen group; keeping the
    // column unobstructed reads better than chips on the visual.

    // Crosshair + readout. Y maps to log-frequency (matching the
    // freq_to_y closure above); X maps to dB delta around the
    // centerline. Z lines show the post-weighting L and R levels at
    // the cursor freq so the readout matches the curve / heatmap.
    if let Some(cursor) = response.hover_pos() {
        if rect.contains(cursor) {
            let freq = y_to_freq_log(cursor.y, freq_min, freq_max, rect);
            let delta_db = (center_x - cursor.x) / px_per_db.max(1e-6);
            let l_raw = sample_bin_db(&state.left_scratch, freq, sr, fft_size);
            let r_raw = sample_bin_db(&state.right_scratch, freq, sr, fft_size);
            let w_db = weighting_db_at(weighting, freq) + weighting_align;
            let l_db = (l_raw + w_db).max(MIN_DB);
            let r_db = (r_raw + w_db).max(MIN_DB);
            let corr = sample_bin_db(&state.correlation_scratch, freq, sr, fft_size);
            let balance = if delta_db.abs() < 0.05 {
                "0.0 dB (centred)".to_string()
            } else if delta_db > 0.0 {
                format!("L +{:.1} dB", delta_db)
            } else {
                format!("R +{:.1} dB", -delta_db)
            };
            let lines = vec![
                format_hz_readout(freq),
                balance,
                format!("L: {:>6.1} dB", l_db),
                format!("R: {:>6.1} dB", r_db),
                format!("corr: {:+.2}", corr.clamp(-1.0, 1.0)),
            ];
            draw_crosshair(&painter, rect, cursor, &lines);
        }
    }
}

/// Per-bin correlation → spectrogram colormap, mapped upside-down so the
/// worst state (anti-phase) reads as the "loudest" color. `+1` = blue,
/// `0` = green, `-0.8` = red, `-1` = white. Piecewise-linear remap onto
/// the spectrogram's 8-stop ramp so white occupies only the last 10% of
/// the correlation range — matching the spectrogram's own red→white tail.
///
/// Slopes are matched (-0.30 on c) across the c=0 and c=-0.8 join points,
/// so the color sweeps smoothly as correlation moves through neutral or
/// into anti-phase — without the visible kink an asymmetric slope
/// produces on uncorrelated stereo.
fn correlation_color_ramp(c: f32) -> egui::Color32 {
    let c = c.clamp(-1.0, 1.0);
    let t = if c >= -0.8 {
        // Single slope -0.30 across c ∈ [-0.8, 1]: c=+1 → 0.40 (cool),
        // c=0 → 0.70 (warm), c=-0.8 → 0.94 (red).
        0.70 - 0.30 * c
    } else {
        // Tail to white at c=-1, slope -0.30 maintained.
        0.94 + 0.30 * (-c - 0.8)
    };
    spectrogram_colormap(t)
}

/// Rust port of the `colormap()` function in `spectrum_line.wgsl`. Kept
/// value-for-value identical so the correlation strip and the spectrogram
/// share a single visual vocabulary.
fn spectrogram_colormap(t_in: f32) -> egui::Color32 {
    let t = t_in.clamp(0.0, 1.0);
    const C0: [f32; 3] = [0.00, 0.00, 0.00];
    const C1: [f32; 3] = [0.00, 0.00, 0.45];
    const C2: [f32; 3] = [0.00, 0.10, 0.95];
    const C3: [f32; 3] = [0.00, 0.80, 0.95];
    const C4: [f32; 3] = [0.20, 0.95, 0.20];
    const C5: [f32; 3] = [0.95, 0.95, 0.00];
    const C6: [f32; 3] = [0.95, 0.00, 0.00];
    const C7: [f32; 3] = [1.00, 1.00, 1.00];
    let mix = |a: [f32; 3], b: [f32; 3], u: f32| -> [f32; 3] {
        [
            a[0] * (1.0 - u) + b[0] * u,
            a[1] * (1.0 - u) + b[1] * u,
            a[2] * (1.0 - u) + b[2] * u,
        ]
    };
    let rgb = if t < 0.15 {
        mix(C0, C1, t / 0.15)
    } else if t < 0.35 {
        mix(C1, C2, (t - 0.15) / 0.20)
    } else if t < 0.55 {
        mix(C2, C3, (t - 0.35) / 0.20)
    } else if t < 0.70 {
        mix(C3, C4, (t - 0.55) / 0.15)
    } else if t < 0.80 {
        mix(C4, C5, (t - 0.70) / 0.10)
    } else if t < 0.90 {
        mix(C5, C6, (t - 0.80) / 0.10)
    } else {
        mix(C6, C7, (t - 0.90) / 0.10)
    };
    let to_u8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgb(to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2]))
}

/// Straight sRGB linear interpolation between two `Color32`s. Used to
/// fade the correlation strip toward a neutral grey in silent regions.
fn lerp_color_rgb(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp_u = |x: u8, y: u8| {
        (x as f32 * (1.0 - t) + y as f32 * t).round().clamp(0.0, 255.0) as u8
    };
    egui::Color32::from_rgb(
        lerp_u(a.r(), b.r()),
        lerp_u(a.g(), b.g()),
        lerp_u(a.b(), b.b()),
    )
}

/// Right-column container: splits vertically into the loudness meter
/// (top row, aligned with the MS plot on the left) and the L/R
/// balance column (bottom row, aligned with the spectrogram so the
/// frequency axis reads consistently across the bottom strip). Split
/// + column width are driven by the grab handle at the cross.
///
/// Both cells get an exact-sized rect + child UI with `set_clip_rect`
/// so their content can't push the parent cursor past the requested
/// height. This is what keeps the spectrogram top edge lined up with
/// the LR cell top edge when the top half is small enough that the
/// loudness readouts would otherwise overflow.
fn draw_right_column(ui: &mut egui::Ui, state: &mut EditorState) {
    let top_fraction = state.shared.top_fraction();
    ui.spacing_mut().item_spacing.y = 0.0;
    let total = ui.available_size();
    let top_h = total.y * top_fraction;
    let bottom_h = total.y - top_h;

    let (top_rect, _) =
        ui.allocate_exact_size(egui::vec2(total.x, top_h), egui::Sense::hover());
    let mut top_child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(top_rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    top_child.set_clip_rect(top_rect);
    draw_loudness_panel(&mut top_child, state);

    let (bot_rect, _) =
        ui.allocate_exact_size(egui::vec2(total.x, bottom_h), egui::Sense::hover());
    let mut bot_child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(bot_rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    bot_child.set_clip_rect(bot_rect);
    draw_lr_column(&mut bot_child, state);
}

/// Floating grab handle at the 2×2 cross. Dragging adjusts both axes
/// of the grid at once: horizontal drag resizes the right column,
/// vertical drag moves the top/bottom split. Double-click resets to
/// the defaults (200 px, 50/50). Rendered as a top-level `Area` so
/// the hit test isn't eaten by whichever panel owns the underlying
/// pixel.
fn draw_layout_grab_handle(ctx: &egui::Context, state: &EditorState) {
    // Two 26 px top bars (refs + controls) and one 26 px bottom bar
    // (spectrogram + L/R controls) bracket the content area; see
    // `create_editor`. Subtract both ends so `content_h` is the actual
    // height of the four-quadrant figure region — without subtracting
    // the footer, `top_frac` reads off the full window height and the
    // handle drifts below the visual cross by half the footer's height.
    let screen = ctx.screen_rect();
    let content_top = screen.top() + TOP_BARS_HEIGHT;
    let content_bottom = screen.bottom() - BOTTOM_BAR_HEIGHT;
    let content_h = (content_bottom - content_top).max(1.0);
    let col_w = state.shared.right_column_width();
    let top_frac = state.shared.top_fraction();

    // Logical cross = the actual panel boundary. Visual cross = where
    // we draw the handle, inset from the window chrome so the macOS
    // bottom-right resize grip (and the other corners) can't swallow
    // drags. Stored layout values stay on the logical path so any one
    // figure can collapse its siblings to zero and take the window.
    const HANDLE_SIZE: f32 = 20.0;
    const EDGE_MARGIN: f32 = 16.0;
    let logical_x = screen.right() - col_w;
    let logical_y = content_top + content_h * top_frac;
    let handle_x = logical_x.clamp(screen.left() + EDGE_MARGIN, screen.right() - EDGE_MARGIN);
    let handle_y = logical_y.clamp(content_top + EDGE_MARGIN, content_bottom - EDGE_MARGIN);
    let handle_rect = egui::Rect::from_center_size(
        egui::pos2(handle_x, handle_y),
        egui::vec2(HANDLE_SIZE, HANDLE_SIZE),
    );

    let area = egui::Area::new(egui::Id::new("analyzer-grab-handle"))
        .order(egui::Order::Foreground)
        .fixed_pos(handle_rect.min);
    area.show(ctx, |ui| {
        let resp = ui.allocate_rect(
            egui::Rect::from_min_size(handle_rect.min, egui::vec2(HANDLE_SIZE, HANDLE_SIZE)),
            egui::Sense::click_and_drag(),
        );

        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
        }

        if resp.dragged() {
            let delta = resp.drag_delta();
            // No chrome clamp here — panels may collapse fully so one
            // figure can take the whole window. The only guard is
            // staying inside the window so stored values remain sane
            // under resize; the visual handle is clamped separately
            // above to keep it clickable.
            let new_col_w = (col_w - delta.x).clamp(0.0, screen.width());
            state.shared.set_right_column_width(new_col_w);
            let new_top_h = content_h * top_frac + delta.y;
            state
                .shared
                .set_top_fraction((new_top_h / content_h).clamp(0.0, 1.0));
        }

        if resp.double_clicked() {
            state.shared.reset_layout();
        }

        let painter = ui.painter();
        let bright = resp.hovered() || resp.dragged();
        let color = if bright {
            egui::Color32::from_rgb(220, 235, 255)
        } else {
            egui::Color32::from_rgb(150, 200, 255)
        };

        // Four-way "move" glyph: solid filled triangular arrowheads
        // pointing N/S/E/W with thin rectangular shafts joining each
        // back toward the centre. Centre stays empty so the icon reads
        // as "drag in any direction" rather than a target reticle.
        let c = handle_rect.center();
        let tip = 9.0_f32;     // outermost point of each arrow
        let base = 5.5_f32;    // arrowhead base distance from centre
        let head_w = 3.5_f32;  // half-width of the arrowhead base
        let shaft_w = 1.25_f32;// half-width of the shaft
        let shaft_in = 1.5_f32;// inner end of the shaft (centre gap)

        // Horizontal shafts.
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(c.x + shaft_in, c.y - shaft_w),
                egui::pos2(c.x + base, c.y + shaft_w),
            ),
            0.0,
            color,
        );
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(c.x - base, c.y - shaft_w),
                egui::pos2(c.x - shaft_in, c.y + shaft_w),
            ),
            0.0,
            color,
        );
        // Vertical shafts.
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(c.x - shaft_w, c.y + shaft_in),
                egui::pos2(c.x + shaft_w, c.y + base),
            ),
            0.0,
            color,
        );
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(c.x - shaft_w, c.y - base),
                egui::pos2(c.x + shaft_w, c.y - shaft_in),
            ),
            0.0,
            color,
        );

        let no_stroke = egui::Stroke::NONE;
        // Right arrowhead.
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(c.x + base, c.y - head_w),
                egui::pos2(c.x + base, c.y + head_w),
                egui::pos2(c.x + tip, c.y),
            ],
            color,
            no_stroke,
        ));
        // Left arrowhead.
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(c.x - base, c.y - head_w),
                egui::pos2(c.x - base, c.y + head_w),
                egui::pos2(c.x - tip, c.y),
            ],
            color,
            no_stroke,
        ));
        // Down arrowhead.
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(c.x - head_w, c.y + base),
                egui::pos2(c.x + head_w, c.y + base),
                egui::pos2(c.x, c.y + tip),
            ],
            color,
            no_stroke,
        ));
        // Up arrowhead.
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(c.x - head_w, c.y - base),
                egui::pos2(c.x + head_w, c.y - base),
                egui::pos2(c.x, c.y - tip),
            ],
            color,
            no_stroke,
        ));
    });
}

/// Total height of the two fixed 26 px top bars (refs + controls).
/// Keep in sync with the `exact_height` calls in `create_editor`.
const TOP_BARS_HEIGHT: f32 = 26.0 * 2.0;
/// Height of the bottom-footer panel (spectrogram + L/R controls).
/// Same 26 px as the top bars so the content area math stays
/// symmetric. Keep in sync with `create_editor`.
const BOTTOM_BAR_HEIGHT: f32 = 26.0;

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
        if ui
            .button("Reset")
            .on_hover_text("Reset Integrated, LRA, and Max holds")
            .clicked()
        {
            state.shared.request_loudness_reset();
        }
        ui.add_space(4.0);

        // Split horizontally: slim meter column on the left, numeric
        // readouts stacked on the right. The meter's content (column
        // + tick marks + labels) is only ~65 px wide, so reserving a
        // fixed narrow slice for it leaves the rest of the panel for
        // readable "Short-term / -6.8 LUFS" rows instead of burning
        // horizontal space on the meter's centring margins.
        let total_avail = ui.available_size();
        const METER_WIDTH: f32 = 80.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let meter_size = egui::vec2(METER_WIDTH.min(total_avail.x), total_avail.y);
            let (rect, _) = ui.allocate_exact_size(meter_size, egui::Sense::hover());
            if meter_size.y > 0.0 && meter_size.x > 0.0 {
                draw_meter_column(ui.painter_at(rect), rect, &snap);
            }
            // First visible loaded ref slot drives the inline-delta
            // comparison column. Matches MS-plot semantics: toggle
            // slot visibility to pick which ref the numbers chase.
            let slots = state.params.ref_slots.read();
            let active_ref = slots.slots.iter().enumerate().find_map(|(i, s)| {
                s.analysis.as_ref().filter(|_| s.visible).map(|a| (i, a))
            });
            let (ref_analysis, ref_color) = match active_ref {
                Some((i, a)) => {
                    let c = REF_SLOT_COLORS[i];
                    (Some(a), egui::Color32::from_rgb(c[0], c[1], c[2]))
                }
                None => (None, egui::Color32::from_gray(120)),
            };
            ui.vertical(|ui| {
                draw_loudness_readouts(ui, &snap, ref_analysis, ref_color);
            });
        });
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
    let label_color = egui::Color32::WHITE;
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
            draw_outlined_text(
                &painter,
                egui::pos2(right_tick_x1 + label_gap, y),
                egui::Align2::LEFT_CENTER,
                &format!("{}", db as i32),
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

/// Warm-up thresholds: seconds of audio required before a metric is
/// considered stable enough to display. Below its threshold, the row
/// shows "…" in place of the number (and suppresses the ref delta)
/// so you don't stare at an LRA of 2 LU that's only there because
/// you've got 4 seconds of material.
const WARMUP_ST_SECS: f32 = 3.0;
const WARMUP_INTEGRATED_SECS: f32 = 10.0;
const WARMUP_LRA_SECS: f32 = 30.0;

/// Animated "loading" placeholder shown while a metric is warming up.
/// Cycles through ".", "..", "..." at ~2.5 Hz (400 ms per step) so
/// the reader can see the meter is still gathering data rather than
/// being frozen. Pulled from `ctx.input().time` so the animation is
/// driven by wall-clock, not audio time — it keeps stepping even
/// when the DAW is paused.
fn warmup_placeholder(ctx: &egui::Context) -> &'static str {
    let t = ctx.input(|i| i.time);
    match ((t * 2.5) as i64).rem_euclid(3) {
        0 => ".  ",
        1 => ".. ",
        _ => "...",
    }
}

fn draw_loudness_readouts(
    ui: &mut egui::Ui,
    snap: &LoudnessSnapshot,
    ref_analysis: Option<&RefAnalysis>,
    ref_color: egui::Color32,
) {
    let elapsed = snap.elapsed_secs;
    let placeholder = warmup_placeholder(ui.ctx());
    // Row labels in a slightly muted white so the values still read
    // as the primary information; values themselves go pure white.
    let label_color = egui::Color32::from_gray(200);
    let value_color = egui::Color32::WHITE;
    let highlight_bg = egui::Color32::from_rgb(40, 60, 110);

    // `delta` is an optional precomputed (live − ref, ref_raw) pair for
    // rows where the ref track provides a matching offline aggregate;
    // rendered to the right of the live value in the slot's colour.
    // Label + value widths are fixed so the column reads as a table.
    let row = |ui: &mut egui::Ui,
               label: &str,
               text: String,
               highlight: bool,
               delta: Option<(f32, f32, DeltaUnit)>| {
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
                        // Right-to-left: delta sits rightmost, then live
                        // value to its left. Keeps the live number in a
                        // predictable screen position while the delta
                        // varies in width.
                        if let Some((delta_val, ref_val, unit)) = delta {
                            let delta_str = fmt_delta(delta_val, unit);
                            // Monospace family — egui's bundled Hack font
                            // covers Δ (U+0394); the proportional default
                            // Ubuntu-Light doesn't, so without monospace the
                            // delta glyph would render as tofu.
                            let resp = ui.add(
                                egui::Label::new(
                                    egui::RichText::new(delta_str)
                                        .color(ref_color)
                                        .monospace()
                                        .size(10.0),
                                )
                                .truncate(),
                            );
                            resp.on_hover_text(format!(
                                "ref: {}",
                                fmt_absolute(ref_val, unit)
                            ));
                        }
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

    // Delta = live − ref, only when both numbers are valid signal (not
    // the MIN_DB sentinel). Returns `None` otherwise so the row renders
    // live-only, same as when no ref is loaded.
    let delta_lufs = |live: f32, reference: f32, unit: DeltaUnit| -> Option<(f32, f32, DeltaUnit)> {
        if live > MIN_DB + 1.0 && reference > MIN_DB + 1.0 {
            Some((live - reference, reference, unit))
        } else {
            None
        }
    };
    let delta_lu = |live: f32, reference: f32| -> Option<(f32, f32, DeltaUnit)> {
        // LU/LRA/DR/PLR never carry a MIN_DB sentinel — they're 0 for
        // "no signal" — so we take the numbers as-is.
        Some((live - reference, reference, DeltaUnit::Lu))
    };

    // Live-only readouts (no offline analogue): short-term, live
    // peak/RMS/correlation. These rows get `None` for delta.
    let ref_int = ref_analysis.map(|r| r.integrated_lufs);
    let ref_lra = ref_analysis.map(|r| r.lra_lu);
    let ref_dr = ref_analysis.map(|r| r.dr_lu());
    let ref_plr = ref_analysis.map(|r| r.plr_lu());
    let ref_st_max = ref_analysis.map(|r| r.short_term_max_lufs);
    let ref_tp_max = ref_analysis.map(|r| r.true_peak_max_dbtp);
    let ref_peak_max = ref_analysis.map(|r| r.sample_peak_max_db);
    let ref_rms_max = ref_analysis.map(|r| r.rms_max_db);

    // Warm-up gating: hide the live value (and its ref delta) until
    // the meter has processed enough audio for the metric to be
    // stable. `warmup` picks between the formatted number and "…";
    // `gate_delta` drops the delta when the live value is still warm.
    let warmup = |threshold: f32, ready: String| -> String {
        if elapsed >= threshold {
            ready
        } else {
            placeholder.to_string()
        }
    };
    let gate_delta = |threshold: f32,
                      d: Option<(f32, f32, DeltaUnit)>|
     -> Option<(f32, f32, DeltaUnit)> {
        if elapsed >= threshold {
            d
        } else {
            None
        }
    };

    // Short-term gets its own oversized hero row — it's the live "now"
    // value the user reads from across a room, so it needs to be the
    // visually dominant readout in the cell. Drawn inline (not via
    // `row`) because the size + emphasis differ from every other row;
    // baseline-centred so the large value and the smaller label sit
    // on the same optical line.
    egui::Frame::new()
        .fill(egui::Color32::TRANSPARENT)
        .inner_margin(egui::Margin {
            left: 4,
            right: 4,
            top: 4,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("Short-term")
                            .color(label_color)
                            .size(14.0)
                            .strong(),
                    )
                    .truncate(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(warmup(
                                WARMUP_ST_SECS,
                                fmt_lufs(snap.short_term_lufs),
                            ))
                            .color(value_color)
                            .size(26.0)
                            .strong(),
                        )
                        .truncate(),
                    );
                });
            });
        });
    // Hairline divider so the hero row reads as its own block above the
    // table of secondary metrics.
    let avail = ui.available_rect_before_wrap();
    let sep_y = ui.cursor().min.y;
    ui.painter().line_segment(
        [
            egui::pos2(avail.left() + 4.0, sep_y),
            egui::pos2(avail.right() - 4.0, sep_y),
        ],
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(40)),
    );
    ui.add_space(2.0);
    row(
        ui,
        "Integrated",
        warmup(WARMUP_INTEGRATED_SECS, fmt_lufs(snap.integrated_lufs)),
        true,
        gate_delta(
            WARMUP_INTEGRATED_SECS,
            ref_int.and_then(|r| delta_lufs(snap.integrated_lufs, r, DeltaUnit::Lu)),
        ),
    );
    row(
        ui,
        "Range",
        warmup(WARMUP_LRA_SECS, fmt_lu(snap.lra_lu)),
        false,
        gate_delta(
            WARMUP_LRA_SECS,
            ref_lra.and_then(|r| delta_lu(snap.lra_lu, r)),
        ),
    );
    row(
        ui,
        "Dynamic",
        warmup(WARMUP_INTEGRATED_SECS, fmt_lu(snap.dr_lu)),
        false,
        gate_delta(
            WARMUP_INTEGRATED_SECS,
            ref_dr.and_then(|r| delta_lu(snap.dr_lu, r)),
        ),
    );
    row(
        ui,
        "PLR",
        warmup(WARMUP_INTEGRATED_SECS, fmt_lu(snap.plr_lu)),
        false,
        gate_delta(
            WARMUP_INTEGRATED_SECS,
            ref_plr.and_then(|r| delta_lu(snap.plr_lu, r)),
        ),
    );
    ui.add_space(4.0);
    row(ui, "Peak", fmt_dbfs(snap.sample_peak_db), false, None);
    row(ui, "RMS", fmt_dbfs(snap.rms_db), false, None);
    row(ui, "Corr", fmt_corr(snap.correlation), false, None);
    ui.add_space(4.0);
    row(ui, "M Max", fmt_lufs(snap.momentary_max_lufs), false, None);
    row(
        ui,
        "ST Max",
        warmup(WARMUP_ST_SECS, fmt_lufs(snap.short_term_max_lufs)),
        false,
        gate_delta(
            WARMUP_ST_SECS,
            ref_st_max.and_then(|r| delta_lufs(snap.short_term_max_lufs, r, DeltaUnit::Lu)),
        ),
    );
    row(
        ui,
        "TP Max",
        fmt_dbtp(snap.true_peak_max_dbtp),
        false,
        ref_tp_max.and_then(|r| delta_lufs(snap.true_peak_max_dbtp, r, DeltaUnit::Db)),
    );
    row(
        ui,
        "Peak Max",
        fmt_dbfs(snap.sample_peak_max_db),
        false,
        ref_peak_max.and_then(|r| delta_lufs(snap.sample_peak_max_db, r, DeltaUnit::Db)),
    );
    row(
        ui,
        "RMS Max",
        fmt_dbfs(snap.rms_max_db),
        false,
        ref_rms_max.and_then(|r| delta_lufs(snap.rms_max_db, r, DeltaUnit::Db)),
    );
}

#[derive(Clone, Copy)]
enum DeltaUnit {
    Lu,
    Db,
}

fn fmt_delta(v: f32, unit: DeltaUnit) -> String {
    let suffix = match unit {
        DeltaUnit::Lu => "LU",
        DeltaUnit::Db => "dB",
    };
    // Δ (U+0394) ships with egui's bundled Hack monospace font but not
    // its Ubuntu-Light proportional font, so the call site renders this
    // string in monospace (see `draw_loudness_readouts`). Without that
    // pairing the glyph would tofu out.
    format!("Δ{:+.1} {}", v, suffix)
}

fn fmt_absolute(v: f32, unit: DeltaUnit) -> String {
    let suffix = match unit {
        DeltaUnit::Lu => "LUFS",
        DeltaUnit::Db => "dB",
    };
    format!("{:+.1} {}", v, suffix)
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

fn fmt_dbfs(v: f32) -> String {
    if v <= -120.0 {
        "-- dBFS".to_string()
    } else {
        format!("{:.1} dBFS", v)
    }
}

fn fmt_corr(v: f32) -> String {
    // +1 = mono, 0 = uncorrelated, -1 = fully out of phase. Two
    // decimals because the useful range for phase checks sits in
    // the last 0.1 of each direction.
    format!("{:+.2}", v)
}

fn draw_controls(ui: &mut egui::Ui, state: &mut EditorState, setter: &ParamSetter) {
    let params = state.params.clone();
    let (bpm_opt, _beat, _playing) = state.shared.transport();
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;

        // ── Beat-Sync group ─────────────────────────────────────────
        let mut sync_val = params.sync.value();
        let host_ready = bpm_opt.is_some();
        ui.add_enabled_ui(host_ready, |ui| {
            let resp = ui.checkbox(&mut sync_val, "Beat-Sync");
            let resp = if host_ready {
                resp.on_hover_text(
                    "Lock the spectrogram to the host's bars/beats grid. Off = scrolls right-to-left at native rate.",
                )
            } else {
                resp.on_hover_text("Host isn't reporting tempo - start playback in Sync-aware host to enable.")
            };
            if resp.changed() {
                setter.begin_set_parameter(&params.sync);
                setter.set_parameter(&params.sync, sync_val);
                setter.end_set_parameter(&params.sync);
            }
        });

        ui.label("Window")
            .on_hover_text("Length of the beat-locked spectrogram window in musical time.");
        let mut window_val = params.sync_window.value();
        egui::ComboBox::from_id_salt("sync-window")
            .selected_text(window_val.label())
            .width(78.0)
            .show_ui(ui, |ui| {
                for opt in [
                    SyncWindow::Beat,
                    SyncWindow::HalfBar,
                    SyncWindow::Bar,
                    SyncWindow::TwoBars,
                    SyncWindow::FourBars,
                    SyncWindow::EightBars,
                    SyncWindow::SixteenBars,
                    SyncWindow::ThirtyTwoBars,
                    SyncWindow::SixtyFourBars,
                ] {
                    if ui
                        .selectable_value(&mut window_val, opt, opt.label())
                        .changed()
                    {
                        setter.begin_set_parameter(&params.sync_window);
                        setter.set_parameter(&params.sync_window, window_val);
                        setter.end_set_parameter(&params.sync_window);
                    }
                }
            });

        ui.separator();

        // Spectrogram controls (source + Sharpen + Floor) and the L/R
        // range live in the bottom footer, directly under the visuals
        // they drive. See `draw_bottom_controls`.

        // ── Frequency range ─────────────────────────────────────────
        ui.label("Range")
            .on_hover_text("Visible frequency range. Both spectrum curves and the spectrogram zoom together.");
        let mut fmin_val = params.freq_min_hz.value();
        if ui
            .add(
                egui::DragValue::new(&mut fmin_val)
                    .range(10..=2000)
                    .speed(1.0)
                    .prefix("Lo "),
            )
            .on_hover_text("Low edge of the visible frequency range, in Hz.")
            .changed()
        {
            setter.begin_set_parameter(&params.freq_min_hz);
            setter.set_parameter(&params.freq_min_hz, fmin_val);
            setter.end_set_parameter(&params.freq_min_hz);
        }
        let mut fmax_val = params.freq_max_hz.value();
        if ui
            .add(
                egui::DragValue::new(&mut fmax_val)
                    .range(1000..=25_000)
                    .speed(10.0)
                    .prefix("Hi "),
            )
            .on_hover_text("High edge of the visible frequency range. Clamped at render time to Nyquist (sample-rate / 2).")
            .changed()
        {
            setter.begin_set_parameter(&params.freq_max_hz);
            setter.set_parameter(&params.freq_max_hz, fmax_val);
            setter.end_set_parameter(&params.freq_max_hz);
        }

        // ── Display weighting + overlay ─────────────────────────────
        ui.label("Weighting")
            .on_hover_text("Frequency tilt applied to both the curves and the spectrogram colourmap. Doesn't affect loudness measurements.");
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
                    if ui
                        .selectable_value(&mut weight_val, opt, opt.label())
                        .on_hover_text(opt.tooltip())
                        .changed()
                    {
                        setter.begin_set_parameter(&params.weighting);
                        setter.set_parameter(&params.weighting, weight_val);
                        setter.end_set_parameter(&params.weighting);
                    }
                }
            });

        ui.label("Overlay")
            .on_hover_text("Optional overlay drawn on top of the MS curves.");
        let mut ref_val = params.reference_curve.value();
        egui::ComboBox::from_id_salt("ref-curve")
            .selected_text(ref_val.label())
            .width(120.0)
            .show_ui(ui, |ui| {
                for opt in [ReferenceCurve::None, ReferenceCurve::Weighting] {
                    if ui
                        .selectable_value(&mut ref_val, opt, opt.label())
                        .on_hover_text(opt.tooltip())
                        .changed()
                    {
                        setter.begin_set_parameter(&params.reference_curve);
                        setter.set_parameter(&params.reference_curve, ref_val);
                        setter.end_set_parameter(&params.reference_curve);
                    }
                }
            });

        ui.label("Smooth")
            .on_hover_text("Frequency-domain smoothing applied to the spectrum curves and reference bands. Wider = smoother shape, less detail.");
        let mut smooth_val = params.freq_smoothing.value();
        egui::ComboBox::from_id_salt("freq-smoothing")
            .selected_text(smooth_val.label())
            .width(120.0)
            .show_ui(ui, |ui| {
                for opt in [
                    FreqSmoothing::None,
                    FreqSmoothing::TwentyFourth,
                    FreqSmoothing::Twelfth,
                    FreqSmoothing::Sixth,
                    FreqSmoothing::Third,
                    FreqSmoothing::Erb,
                ] {
                    if ui
                        .selectable_value(&mut smooth_val, opt, opt.label())
                        .on_hover_text(opt.tooltip())
                        .changed()
                    {
                        setter.begin_set_parameter(&params.freq_smoothing);
                        setter.set_parameter(&params.freq_smoothing, smooth_val);
                        setter.end_set_parameter(&params.freq_smoothing);
                    }
                }
            });

        ui.label("FFT")
            .on_hover_text("FFT window size, in samples. Larger = finer pitch resolution; smaller = faster transient response.");
        let mut fft_val = params.fft_size.value();
        egui::ComboBox::from_id_salt("fft-size")
            .selected_text(fft_val.label())
            .width(54.0)
            .show_ui(ui, |ui| {
                for opt in [
                    FftSize::K2,
                    FftSize::K4,
                    FftSize::K8,
                    FftSize::K16,
                    FftSize::K32,
                ] {
                    if ui
                        .selectable_value(&mut fft_val, opt, opt.label())
                        .on_hover_text(opt.tooltip())
                        .changed()
                    {
                        setter.begin_set_parameter(&params.fft_size);
                        setter.set_parameter(&params.fft_size, fft_val);
                        setter.end_set_parameter(&params.fft_size);
                    }
                }
            });

    });
}

/// Bottom-footer toolbar. Holds the controls that drive the
/// bottom-anchored visuals: the spectrogram source switch + Sharpen
/// group on the left (under the spectrogram cell), and the L/R range
/// on the right (under the L/R column). Same row height + frame fill
/// as the top header so the two read as a matched pair.
fn draw_bottom_controls(ui: &mut egui::Ui, state: &mut EditorState, setter: &ParamSetter) {
    let params = state.params.clone();
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;

        // ── Spectrogram source + Sharpen group ──────────────────────
        ui.label(egui::RichText::new("Spec").color(egui::Color32::from_gray(160)))
            .on_hover_text("Spectrogram-only controls.");
        let current_src = state.shared.spectrogram_source();
        for &mode in &[
            SpectrogramSource::Mid,
            SpectrogramSource::Side,
            SpectrogramSource::LeftRight,
        ] {
            let selected = mode == current_src;
            let label = mode.chip_label();
            let resp = ui
                .add(egui::SelectableLabel::new(
                    selected,
                    egui::RichText::new(label).monospace(),
                ))
                .on_hover_text(match mode {
                    SpectrogramSource::Mid => {
                        "Mid: (L+R)/2 - full-height spectrogram of the centre/sum signal."
                    }
                    SpectrogramSource::Side => {
                        "Side: (L-R)/2 - full-height spectrogram of the stereo difference. Centre content disappears."
                    }
                    SpectrogramSource::LeftRight => {
                        "L | R: stacked Left (top) over Right (bottom). Compare channels at a glance."
                    }
                });
            if resp.clicked() && !selected {
                state.shared.set_spectrogram_source(mode);
            }
        }

        let mut ss_val = params.synchrosqueeze.value();
        if ui
            .checkbox(&mut ss_val, "Sharpen")
            .on_hover_text(
                "Phase-reassigns spectrogram energy to its true frequency. Tonal lines tighten into single pixels; broadband noise stays soft. Toggling clears the spectrogram so before/after stays unambiguous.",
            )
            .changed()
        {
            setter.begin_set_parameter(&params.synchrosqueeze);
            setter.set_parameter(&params.synchrosqueeze, ss_val);
            setter.end_set_parameter(&params.synchrosqueeze);
        }
        ui.add_enabled_ui(ss_val, |ui| {
            ui.label("Floor")
                .on_hover_text(
                    "Power threshold for Sharpen. Bins below this don't contribute. Lower = transients survive; higher = cleaner on noise.",
                );
            let mut gate_val = params.synchro_gate_db.value();
            if ui
                .add(
                    egui::DragValue::new(&mut gate_val)
                        .range(SS_GATE_DB_MIN..=SS_GATE_DB_MAX)
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

        // ── L/R column range, right-aligned under the L/R cell ──────
        // Push to the right edge so it sits under the right-side
        // L/R column, matching the spectrogram chips on the left
        // sitting under the spectrogram cell.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut lr_range_val = params.lr_range_db.value();
            if ui
                .add(
                    egui::DragValue::new(&mut lr_range_val)
                        .range(5..=60)
                        .speed(0.25)
                        .suffix(" dB"),
                )
                .on_hover_text(
                    "Centre of the L/R column = perfect balance. Outer edges = this many dB of imbalance toward Left or Right.",
                )
                .changed()
            {
                setter.begin_set_parameter(&params.lr_range_db);
                setter.set_parameter(&params.lr_range_db, lr_range_val);
                setter.end_set_parameter(&params.lr_range_db);
            }
            ui.label("L/R +/-")
                .on_hover_text("Half-range of the L/R balance axis in dB.");
        });
    });
}

fn draw_ref_slots(ui: &mut egui::Ui, state: &mut EditorState) {
    let params = state.params.clone();
    let shared = state.shared.clone();
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(egui::RichText::new("Refs").color(egui::Color32::from_gray(180)));
        for slot_idx in 0..REF_SLOT_COUNT {
            draw_single_ref_slot(ui, &params.ref_slots, &shared, slot_idx);
            if slot_idx < REF_SLOT_COUNT - 1 {
                ui.separator();
            }
        }
    });
}

fn draw_single_ref_slot(
    ui: &mut egui::Ui,
    ref_slots: &Arc<RwLock<RefSlots>>,
    shared: &Arc<AnalyzerGuiShared>,
    slot_idx: usize,
) {
    let color = REF_SLOT_COLORS[slot_idx];
    let color32 = egui::Color32::from_rgb(color[0], color[1], color[2]);

    // Snapshot slot state so we release the lock before any file-dialog
    // work: rfd's native picker can block for seconds, and the analysis
    // worker takes a `write` lock when it finishes.
    let (name, loaded, visible) = {
        let slots = ref_slots.read();
        let slot = &slots.slots[slot_idx];
        (slot.name.clone(), slot.is_loaded(), slot.visible)
    };
    let analyzing = shared.is_ref_analyzing(slot_idx);

    ui.label(
        egui::RichText::new(format!("{}", slot_idx + 1))
            .color(egui::Color32::from_gray(150))
            .monospace(),
    );

    if analyzing {
        ui.label(
            egui::RichText::new("analysing...")
                .color(egui::Color32::from_gray(200))
                .italics(),
        );
        return;
    }

    if !loaded {
        if ui.small_button("Load...").clicked() {
            launch_ref_picker(slot_idx, ref_slots.clone(), shared.clone());
        }
        return;
    }

    // Colour dot — dim when visibility is off.
    let dot_color = if visible {
        color32
    } else {
        egui::Color32::from_rgba_premultiplied(
            (color[0] as f32 * 0.35) as u8,
            (color[1] as f32 * 0.35) as u8,
            (color[2] as f32 * 0.35) as u8,
            255,
        )
    };
    let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(dot_rect.center(), 5.0, dot_color);

    let label_color = if visible {
        egui::Color32::WHITE
    } else {
        egui::Color32::from_white_alpha(120)
    };
    let truncated = truncate_name(&name, 16);
    let btn = ui
        .add(egui::Button::new(
            egui::RichText::new(truncated).color(label_color),
        ))
        .on_hover_text(format!("{name}\nclick to toggle visibility"));
    if btn.clicked() {
        let mut slots = ref_slots.write();
        slots.slots[slot_idx].visible = !slots.slots[slot_idx].visible;
    }
    if ui.small_button("x").on_hover_text("Clear slot").clicked() {
        let mut slots = ref_slots.write();
        slots.slots[slot_idx] = RefSlot::default();
    }
}

fn truncate_name(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let keep = max_chars.saturating_sub(1);
        let mut out: String = s.chars().take(keep).collect();
        out.push_str("...");
        out
    }
}

/// Spawn a worker thread that (a) runs the non-blocking
/// `AsyncFileDialog` — which dispatches NSOpenPanel onto the main
/// runloop without spinning a nested modal loop, avoiding a baseview
/// RefCell-reentrancy crash — and (b) runs the offline analysis once
/// the user picks a file, writing the result back into the persistent
/// slot.
///
/// On macOS we capture the editor's key NSView on the main thread
/// before spawning so rfd can attach the picker as a sheet on the
/// plugin window — otherwise the panel floats *below* the editor
/// because the editor's NSWindow level sits above the default
/// panel level and the user never sees it.
fn launch_ref_picker(
    slot_idx: usize,
    ref_slots: Arc<RwLock<RefSlots>>,
    shared: Arc<AnalyzerGuiShared>,
) {
    // Main-thread capture of the parent view pointer. Must happen here
    // (not inside the worker) because `NSApplication::sharedApplication`
    // requires a `MainThreadMarker`.
    #[cfg(target_os = "macos")]
    let parent = macos::capture_key_window_parent();
    #[cfg(not(target_os = "macos"))]
    let parent: Option<()> = None;

    shared.set_ref_analyzing(slot_idx, true);
    std::thread::spawn(move || {
        let dialog = rfd::AsyncFileDialog::new()
            .set_title(format!("Load reference for slot {}", slot_idx + 1))
            .add_filter(
                "Audio",
                &["wav", "mp3", "flac", "aac", "m4a", "ogg", "aiff", "aif"],
            );
        #[cfg(target_os = "macos")]
        let dialog = if let Some(p) = parent.as_ref() {
            dialog.set_parent(p)
        } else {
            dialog
        };
        #[cfg(not(target_os = "macos"))]
        let _ = parent;

        let picked = pollster::block_on(dialog.pick_file());
        let Some(handle) = picked else {
            shared.set_ref_analyzing(slot_idx, false);
            return;
        };
        let path = handle.path().to_path_buf();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(String::from)
            .unwrap_or_else(|| format!("ref-{}", slot_idx + 1));

        match analyze_ref_file(&path) {
            Ok(analysis) => {
                let mut slots = ref_slots.write();
                slots.slots[slot_idx] = RefSlot {
                    name,
                    analysis: Some(analysis),
                    visible: true,
                };
            }
            Err(e) => {
                eprintln!("manifold-analyzer: reference analysis failed ({e})");
            }
        }
        shared.set_ref_analyzing(slot_idx, false);
    });
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Id;
    use objc2_app_kit::{NSApplication, NSView};
    use objc2_foundation::MainThreadMarker;
    use raw_window_handle::{
        AppKitDisplayHandle, AppKitWindowHandle, DisplayHandle, HasDisplayHandle,
        HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
    };
    use std::ptr::NonNull;

    /// Owns a retained `NSView` so the pointer we hand to `set_parent`
    /// stays alive across the `AsyncFileDialog` await. Send/Sync are
    /// hand-asserted because `Id<NSView>` isn't auto-`Send`, but we
    /// only read its address on the worker thread — never call
    /// AppKit methods through it off-main-thread.
    pub struct KeyWindowParent {
        view: Id<NSView>,
    }
    unsafe impl Send for KeyWindowParent {}
    unsafe impl Sync for KeyWindowParent {}

    impl HasWindowHandle for KeyWindowParent {
        fn window_handle(
            &self,
        ) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
            let ptr: *const NSView = &*self.view;
            let nn = NonNull::new(ptr as *mut std::ffi::c_void)
                .ok_or(raw_window_handle::HandleError::Unavailable)?;
            let handle = AppKitWindowHandle::new(nn);
            // SAFETY: the `NSView` is retained for the lifetime of
            // `self`, so the raw pointer is valid for this borrow.
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AppKit(handle)) })
        }
    }

    impl HasDisplayHandle for KeyWindowParent {
        fn display_handle(
            &self,
        ) -> Result<DisplayHandle<'_>, raw_window_handle::HandleError> {
            // SAFETY: AppKitDisplayHandle carries no data to invalidate.
            Ok(unsafe {
                DisplayHandle::borrow_raw(RawDisplayHandle::AppKit(AppKitDisplayHandle::new()))
            })
        }
    }

    /// Grab the current key window's `contentView`. Returns `None` off
    /// the main thread or when the plugin has no key window (host not
    /// active, editor not focused).
    pub fn capture_key_window_parent() -> Option<KeyWindowParent> {
        let mtm = MainThreadMarker::new()?;
        let app = NSApplication::sharedApplication(mtm);
        let key_win = app.keyWindow()?;
        let view = key_win.contentView()?;
        Some(KeyWindowParent { view })
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

fn x_to_freq(x: f32, fmin: f32, fmax: f32, rect: egui::Rect) -> f32 {
    let t = ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
    fmin * (fmax / fmin).powf(t)
}

fn y_to_db(y: f32, dmin: f32, dmax: f32, rect: egui::Rect) -> f32 {
    let t = 1.0 - ((y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
    dmin + t * (dmax - dmin)
}

fn y_to_freq_log(y: f32, fmin: f32, fmax: f32, rect: egui::Rect) -> f32 {
    let t = 1.0 - ((y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
    fmin * (fmax / fmin).powf(t)
}

/// Look up a per-bin dB value at an arbitrary frequency with linear bin
/// interpolation. Used by the cursor readout — fast, single-tap-per-call;
/// no smoothing window, so the value is the raw averaged FFT level the
/// audio thread published (the on-screen smoothing/weighting are layered
/// on top in the shader).
fn sample_bin_db(scratch: &[f32], freq: f32, sr: f32, fft_size: usize) -> f32 {
    if scratch.is_empty() || sr <= 0.0 || fft_size == 0 {
        return MIN_DB;
    }
    let bin_per_hz = fft_size as f32 / sr;
    let max_bin = (scratch.len() - 1) as f32;
    let bin_f = (freq * bin_per_hz).clamp(0.0, max_bin);
    let b0 = bin_f.floor() as usize;
    let b1 = (b0 + 1).min(scratch.len() - 1);
    let frac = bin_f - b0 as f32;
    scratch[b0] * (1.0 - frac) + scratch[b1] * frac
}

/// Format a frequency for the cursor readout. Sub-1k uses Hz, ≥1k uses
/// kHz with one decimal where it adds detail (so "1.5k" survives but
/// "2k" doesn't get a needless ".0").
fn format_hz_readout(freq: f32) -> String {
    if freq >= 10000.0 {
        format!("{:.1} kHz", freq / 1000.0)
    } else if freq >= 1000.0 {
        format!("{:.2} kHz", freq / 1000.0)
    } else if freq >= 100.0 {
        format!("{:.0} Hz", freq)
    } else {
        format!("{:.1} Hz", freq)
    }
}

/// Crosshair + tooltip-style readout drawn over a plot when the mouse
/// hovers it. Lines stack vertically inside a small panel near the
/// cursor; the panel flips quadrant if the default position would clip
/// outside `rect`.
fn draw_crosshair(
    painter: &egui::Painter,
    rect: egui::Rect,
    cursor: egui::Pos2,
    lines: &[String],
) {
    let line_color = egui::Color32::from_white_alpha(170);
    painter.line_segment(
        [
            egui::pos2(rect.left(), cursor.y),
            egui::pos2(rect.right(), cursor.y),
        ],
        egui::Stroke::new(1.0, line_color),
    );
    painter.line_segment(
        [
            egui::pos2(cursor.x, rect.top()),
            egui::pos2(cursor.x, rect.bottom()),
        ],
        egui::Stroke::new(1.0, line_color),
    );
    painter.circle_filled(cursor, 2.5, egui::Color32::WHITE);

    if lines.is_empty() {
        return;
    }
    let font = egui::FontId::monospace(11.0);
    let line_h = 14.0;
    let pad = egui::vec2(6.0, 4.0);
    let max_w = lines
        .iter()
        .map(|s| {
            painter
                .layout_no_wrap(s.clone(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    let box_size = egui::vec2(max_w + pad.x * 2.0, line_h * lines.len() as f32 + pad.y * 2.0);
    let mut origin = cursor + egui::vec2(12.0, -box_size.y - 12.0);
    if origin.x + box_size.x > rect.right() - 2.0 {
        origin.x = cursor.x - box_size.x - 12.0;
    }
    if origin.x < rect.left() + 2.0 {
        origin.x = rect.left() + 2.0;
    }
    if origin.y < rect.top() + 2.0 {
        origin.y = cursor.y + 12.0;
    }
    if origin.y + box_size.y > rect.bottom() - 2.0 {
        origin.y = (rect.bottom() - 2.0 - box_size.y).max(rect.top() + 2.0);
    }
    let box_rect = egui::Rect::from_min_size(origin, box_size);
    painter.rect_filled(box_rect, 3.0, egui::Color32::from_black_alpha(220));
    painter.rect_stroke(
        box_rect,
        3.0,
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(120)),
        egui::StrokeKind::Inside,
    );
    for (i, s) in lines.iter().enumerate() {
        painter.text(
            origin + pad + egui::vec2(0.0, i as f32 * line_h),
            egui::Align2::LEFT_TOP,
            s,
            font.clone(),
            egui::Color32::WHITE,
        );
    }
}
