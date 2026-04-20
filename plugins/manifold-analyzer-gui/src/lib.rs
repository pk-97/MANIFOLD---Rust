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

mod gl_paint;
mod gpu_bridge;
mod raw_ring;
mod spectrum_gpu;

use gl_paint::{PainterState, QuadPainter, SharedPainterState};
use manifold_analyzer_dsp::MIN_DB;
use manifold_gpu::GpuDevice;
use nih_plug::prelude::*;
use nih_plug_egui::{EguiState, create_egui_editor, egui};
use raw_ring::RawFrameRing;
use spectrum_gpu::{DisplayConfig, SpectrumGpuRenderer};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
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
const FREQ_MIN: f32 = 10.0;
const FREQ_MAX_LIMIT: f32 = 25_000.0;
const DB_MIN: f32 = -90.0;
const DB_MAX: f32 = -10.0;
const SLOPE_DB_PER_OCT: f32 = 4.5;
const SLOPE_REF_FREQ: f32 = 1000.0;
const ALIGN_OFFSET_DB: f32 = 0.0; // Placeholder for SPAN "Align 0 dB" pink-noise calibration.
const SMOOTH_HALF_OCT_LOG2: f32 = 1.0 / 24.0; // 1/12-oct bandwidth → ±1/24 oct half-width
const FILL_ALPHA: f32 = 0.45;
// Top N% of the render target = spectrum curves, bottom = scrolling
// Mid-only spectrogram. 0.55 leaves a readable spectrogram at the default
// 450 px window height (~200 px strip) without crushing the curves.
const SPECTRUM_FRACTION: f32 = 0.55;
// Vision 4X "Heatmap" default range — colourmap spans these dB values.
const SPECTROGRAM_DB_MIN: f32 = -59.0;
const SPECTROGRAM_DB_MAX: f32 = 0.0;

/// Ring depth for un-averaged frames. At 8192 FFT / 95 % overlap / 48 kHz
/// the producer runs at ~117 Hz and the consumer at ~60 Hz, so pending
/// depth sits near 2 most of the time. 16 tolerates a sluggish GUI frame
/// (~130 ms stall) before the audio thread starts dropping frames.
const RAW_RING_CAPACITY: usize = 16;

pub struct AnalyzerGuiShared {
    sample_rate_bits: AtomicU32,
    fft_size: AtomicUsize,
    /// Averaged (SPAN-style) Mid/Side spectra for the top curve.
    pub mid_db: Mutex<Vec<f32>>,
    pub side_db: Mutex<Vec<f32>>,
    /// Un-averaged Mid frames queued for the spectrogram. Pushed on every
    /// FFT hop by the audio thread, drained once per render by the GUI so
    /// no frames are lost to the producer/consumer rate mismatch.
    pub mid_raw_ring: RawFrameRing,
}

impl AnalyzerGuiShared {
    pub fn new(sample_rate: f32, fft_size: usize) -> Self {
        let num_bins = fft_size / 2;
        Self {
            sample_rate_bits: AtomicU32::new(sample_rate.to_bits()),
            fft_size: AtomicUsize::new(fft_size),
            mid_db: Mutex::new(vec![MIN_DB; num_bins]),
            side_db: Mutex::new(vec![MIN_DB; num_bins]),
            mid_raw_ring: RawFrameRing::new(RAW_RING_CAPACITY, num_bins, MIN_DB),
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
}

struct EditorState {
    shared: Arc<AnalyzerGuiShared>,
    device: Option<GpuDevice>,
    spectrum: Option<SpectrumGpuRenderer>,
    quad: SharedPainterState,
    mid_scratch: Vec<f32>,
    side_scratch: Vec<f32>,
}

pub fn create_editor(
    egui_state: Arc<EguiState>,
    shared: Arc<AnalyzerGuiShared>,
) -> Option<Box<dyn Editor>> {
    let num_bins = shared.fft_size() / 2;
    let state = EditorState {
        shared,
        device: None,
        spectrum: None,
        quad: Arc::new(Mutex::new(PainterState::NotYet)),
        mid_scratch: vec![MIN_DB; num_bins],
        side_scratch: vec![MIN_DB; num_bins],
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
        |ctx, _setter, state| {
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
            if let Some(mut spec) = SpectrumGpuRenderer::new(
                device,
                INITIAL_SPECTRUM_W,
                INITIAL_SPECTRUM_H,
                num_bins,
            ) {
                spec.set_display(DisplayConfig {
                    slope_db_per_oct: SLOPE_DB_PER_OCT,
                    slope_ref_freq: SLOPE_REF_FREQ,
                    align_offset_db: ALIGN_OFFSET_DB,
                    smooth_half_oct_log2: SMOOTH_HALF_OCT_LOG2,
                    fill_alpha: FILL_ALPHA,
                    spectrum_fraction: SPECTRUM_FRACTION,
                    spectrogram_db_min: SPECTROGRAM_DB_MIN,
                    spectrogram_db_max: SPECTROGRAM_DB_MAX,
                });
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

    // Copy latest averaged mid/side spectra for the curves. Drain the raw
    // ring and push every pending frame into the spectrogram history so
    // each FFT hop becomes its own column (matches Vision's temporal
    // density instead of down-sampling to render rate).
    if let (Some(device), Some(spec)) = (state.device.as_ref(), state.spectrum.as_mut()) {
        if let Ok(guard) = state.shared.mid_db.lock() {
            state.mid_scratch.copy_from_slice(&guard);
        }
        if let Ok(guard) = state.shared.side_db.lock() {
            state.side_scratch.copy_from_slice(&guard);
        }
        state.shared.mid_raw_ring.drain(|frame| {
            spec.push_spectrogram_frame(frame);
        });
        let freq_max = (sr * 0.5).clamp(FREQ_MIN * 2.0, FREQ_MAX_LIMIT);
        spec.render(
            device,
            &state.mid_scratch,
            &state.side_scratch,
            sr,
            FREQ_MIN,
            freq_max,
            DB_MIN,
            DB_MAX,
        );
    }

    // Allocate the rect for the whole spectrum view.
    let (rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let freq_max = (sr * 0.5).clamp(FREQ_MIN * 2.0, FREQ_MAX_LIMIT);

    // Spectrum-region rect (top). The spectrogram below is opaque, so grid
    // lines + labels only belong inside this sub-rect — anything drawn below
    // it would be hidden behind the colourmap.
    let spectrum_rect = egui::Rect::from_min_max(
        rect.min,
        egui::pos2(rect.max.x, rect.top() + rect.height() * SPECTRUM_FRACTION),
    );

    // 1. Grid lines underneath the spectrum (so the line sits on top).
    let grid_color = egui::Color32::from_gray(40);
    let label_color = egui::Color32::from_gray(140);

    for &freq in &[100.0_f32, 1000.0, 10_000.0] {
        if freq <= freq_max {
            let x = freq_to_x(freq, FREQ_MIN, freq_max, spectrum_rect);
            painter.line_segment(
                [
                    egui::pos2(x, spectrum_rect.top()),
                    egui::pos2(x, spectrum_rect.bottom()),
                ],
                (1.0, grid_color),
            );
        }
    }
    for db in [-20.0_f32, -40.0, -60.0, -80.0] {
        let y = db_to_y(db, DB_MIN, DB_MAX, spectrum_rect);
        painter.line_segment(
            [
                egui::pos2(spectrum_rect.left(), y),
                egui::pos2(spectrum_rect.right(), y),
            ],
            (1.0, grid_color),
        );
    }

    // 2. PaintCallback: GPU-rendered spectrum line on top of the grid.
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

    // 3. Text labels on top of everything, so they stay readable even when
    //    the spectrum line crosses them. Labels live inside the spectrum
    //    sub-rect (transparent background); the spectrogram below is opaque
    //    and would hide anything painted into it.
    for &freq in &[100.0_f32, 1000.0, 10_000.0] {
        if freq <= freq_max {
            let x = freq_to_x(freq, FREQ_MIN, freq_max, spectrum_rect);
            let label = if freq >= 1000.0 {
                format!("{}k", (freq / 1000.0) as i32)
            } else {
                format!("{}", freq as i32)
            };
            painter.text(
                egui::pos2(x + 4.0, spectrum_rect.bottom() - 4.0),
                egui::Align2::LEFT_BOTTOM,
                label,
                egui::FontId::monospace(10.0),
                label_color,
            );
        }
    }
    for db in [-20.0_f32, -40.0, -60.0, -80.0] {
        let y = db_to_y(db, DB_MIN, DB_MAX, spectrum_rect);
        painter.text(
            egui::pos2(spectrum_rect.left() + 4.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{} dB", db as i32),
            egui::FontId::monospace(10.0),
            label_color,
        );
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
