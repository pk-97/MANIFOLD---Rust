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
mod sample_ring;
mod spectrum_gpu;

use gl_paint::{PainterState, QuadPainter, SharedPainterState};
use manifold_analyzer_dsp::MIN_DB;
use manifold_gpu::GpuDevice;
use nih_plug::prelude::*;
use nih_plug_egui::{EguiState, create_egui_editor, egui};
use sample_ring::SampleRing;
use spectrum_gpu::{DisplayConfig, SpectrumGpuRenderer, SyncConfig};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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
const DB_MAX: f32 = -10.0;
const SLOPE_DB_PER_OCT: f32 = 4.5;
const SLOPE_REF_FREQ: f32 = 1000.0;
const ALIGN_OFFSET_DB: f32 = 0.0; // Placeholder for SPAN "Align 0 dB" pink-noise calibration.
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
/// 10 dB minors. Range must match DB_MIN..DB_MAX below.
const DB_MAJORS: &[f32] = &[-20.0, -40.0, -60.0, -80.0];
const DB_MINORS: &[f32] = &[-30.0, -50.0, -70.0];

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
    pub mid_db: Mutex<Vec<f32>>,
    pub side_db: Mutex<Vec<f32>>,
    /// Raw mid-channel audio samples for the CQT spectrogram. Audio
    /// thread pushes every sample; GUI thread drains and feeds the CQT
    /// pipeline. No FFT on the audio thread for this path.
    pub mid_sample_ring: SampleRing,
    /// Host transport snapshot, published from the audio thread each
    /// process block. `NaN` means "host did not provide this value" — we
    /// can only honour Sync mode when both `bpm_bits` and
    /// `beat_pos_bits` are finite.
    bpm_bits: AtomicU64,
    beat_pos_bits: AtomicU64,
    playing: AtomicBool,
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
            bpm_bits: AtomicU64::new(f64::NAN.to_bits()),
            beat_pos_bits: AtomicU64::new(f64::NAN.to_bits()),
            playing: AtomicBool::new(false),
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
}

struct EditorState {
    shared: Arc<AnalyzerGuiShared>,
    params: Arc<AnalyzerParams>,
    device: Option<GpuDevice>,
    spectrum: Option<SpectrumGpuRenderer>,
    quad: SharedPainterState,
    mid_scratch: Vec<f32>,
    side_scratch: Vec<f32>,
    /// Scratch buffer that accumulates samples drained from the ring
    /// each render frame before they're fed into the CQT pipeline.
    sample_scratch: Vec<f32>,
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
        quad: Arc::new(Mutex::new(PainterState::NotYet)),
        mid_scratch: vec![MIN_DB; num_bins],
        side_scratch: vec![MIN_DB; num_bins],
        sample_scratch: Vec::with_capacity(4096),
    };
    create_egui_editor(
        egui_state,
        state,
        // Build callback runs on each editor spawn (open/close cycle). The
        // prior GL context is gone, so the cached `QuadPainter`'s GL handles
        // (program/VAO/texture) are now dangling — reset the lifecycle so
        // the next PaintCallback rebuilds against the fresh context.
        |_ctx, state: &mut EditorState| {
            *state.quad.lock().unwrap() = PainterState::NotYet;
        },
        |ctx, setter, state| {
            egui::TopBottomPanel::top("analyzer-controls")
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(18, 22, 30)))
                .exact_height(26.0)
                .show(ctx, |ui| {
                    draw_controls(ui, state, setter);
                });
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(8, 10, 14)))
                .show(ctx, |ui| {
                    draw_spectrum(ui, state);
                });
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
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
    if state.spectrum.is_none() {
        if let Some(device) = state.device.as_ref() {
            if let Some(spec) = SpectrumGpuRenderer::new(
                device,
                INITIAL_SPECTRUM_W,
                INITIAL_SPECTRUM_H,
                num_bins,
                sr,
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
            let mut lock = state.quad.lock().unwrap();
            let prev = std::mem::replace(&mut *lock, PainterState::NotYet);
            *lock = match prev {
                PainterState::Ready(qp) => PainterState::PendingDestroy(qp),
                other => other,
            };
        }
    }

    // Copy latest averaged mid/side spectra for the curves; drain new
    // mid audio samples and feed them to the CQT pipeline so every hop
    // boundary emits a new spectrogram column.
    if let (Some(device), Some(spec)) = (state.device.as_ref(), state.spectrum.as_mut()) {
        if let Ok(guard) = state.shared.mid_db.lock() {
            state.mid_scratch.copy_from_slice(&guard);
        }
        if let Ok(guard) = state.shared.side_db.lock() {
            state.side_scratch.copy_from_slice(&guard);
        }
        let ss_on = state.params.synchrosqueeze.value();
        let top_fraction = state.params.top_ratio.value().fraction();
        spec.set_display(DisplayConfig {
            slope_db_per_oct: SLOPE_DB_PER_OCT,
            slope_ref_freq: SLOPE_REF_FREQ,
            align_offset_db: ALIGN_OFFSET_DB,
            smooth_half_oct_log2: SMOOTH_HALF_OCT_LOG2,
            fill_alpha: FILL_ALPHA,
            spectrum_fraction: top_fraction,
            spectrogram_db_min: SPECTROGRAM_DB_MIN,
            spectrogram_db_max: if ss_on { SPECTROGRAM_DB_MAX_SS } else { SPECTROGRAM_DB_MAX_RAW },
            enable_synchrosqueezing: ss_on,
            enable_coherence_check: state.params.coherence.value(),
            synchro_gate_db: state.params.synchro_gate_db.value(),
        });

        let (bpm_opt, beat_opt, _playing) = state.shared.transport();
        let sync_requested = state.params.sync.value();
        let factor_ratio = state.params.sync_factor.value().as_ratio();
        let multiplier = state.params.sync_multiplier.value() as f32;
        let beats_per_window = factor_ratio * multiplier * BEATS_PER_BAR;
        let sync_config = match (sync_requested, bpm_opt, beat_opt) {
            (true, Some(bpm), Some(beat)) if bpm > 0.0 && beats_per_window > 0.0 => SyncConfig {
                enabled: true,
                bpm: bpm as f32,
                beat_pos: beat,
                beats_per_window,
            },
            _ => SyncConfig::OFF,
        };
        spec.set_sync(sync_config);

        state.sample_scratch.clear();
        state.shared.mid_sample_ring.drain_into(&mut state.sample_scratch);
        spec.ingest_samples(&state.sample_scratch);
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
                let mut lock = quad.lock().unwrap();

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
        BEATS_PER_BAR as f32
    } else if px_per_beat >= 3.0 {
        BEATS_PER_BAR as f32 * 2.0
    } else {
        BEATS_PER_BAR as f32 * 4.0
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
