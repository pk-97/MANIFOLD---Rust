//! egui-based GUI for the Manifold Analyzer plugin.
//!
//! Spectrum is rendered in egui (temporarily). A second region demonstrates the
//! **zero-copy manifold-gpu → egui bridge** via IOSurface: Metal renders into
//! an IOSurface-backed texture, GL binds the same IOSurface as a GL_TEXTURE_2D,
//! egui samples it with a custom PaintCallback. No CPU round-trip.
//!
//! # TODO(manifold-gpu-migration)
//!
//! The egui spectrum line below is temporary. Once the zero-copy bridge is
//! proven (which is what this commit establishes), the spectrum line and
//! every other visual (spectrogram, difference, reference overlay) migrates
//! to `manifold-gpu` via the same bridge. egui stays only for chrome —
//! buttons, text, sliders, menus, layout. See MEMORY:
//! `project_analyzer_gpu_migration.md`.
//!
//! # Audio thread ↔ GUI thread
//!
//! Audio thread publishes the spectrum via `try_lock` on
//! `AnalyzerGuiShared::spectrum_db` — drops the update on contention. GUI
//! thread briefly clones under the lock.

mod gl_paint;
mod gpu_bridge;

use gl_paint::{PainterState, QuadPainter, SharedPainterState};
use gpu_bridge::IoSurfaceMtlTexture;
use manifold_analyzer_dsp::MIN_DB;
use manifold_gpu::GpuDevice;
use nih_plug::prelude::*;
use nih_plug_egui::{EguiState, create_egui_editor, egui};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct AnalyzerGuiShared {
    sample_rate_bits: AtomicU32,
    fft_size: AtomicUsize,
    pub spectrum_db: Mutex<Vec<f32>>,
}

impl AnalyzerGuiShared {
    pub fn new(sample_rate: f32, fft_size: usize) -> Self {
        Self {
            sample_rate_bits: AtomicU32::new(sample_rate.to_bits()),
            fft_size: AtomicUsize::new(fft_size),
            spectrum_db: Mutex::new(vec![MIN_DB; fft_size / 2]),
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

/// Per-editor state.
struct EditorState {
    shared: Arc<AnalyzerGuiShared>,
    gpu: Option<PluginGpuRenderer>,
    quad: SharedPainterState,
    start: Instant,
}

pub fn create_editor(
    egui_state: Arc<EguiState>,
    shared: Arc<AnalyzerGuiShared>,
) -> Option<Box<dyn Editor>> {
    let state = EditorState {
        shared,
        gpu: None,
        quad: Arc::new(Mutex::new(PainterState::NotYet)),
        start: Instant::now(),
    };
    create_egui_editor(
        egui_state,
        state,
        |_, _| {},
        |ctx, _setter, state| {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(8, 10, 14)))
                .show(ctx, |ui| {
                    draw_gpu_demo_strip(ui, state);
                    ui.add_space(6.0);

                    let sr = state.shared.sample_rate();
                    let fft_size = state.shared.fft_size();
                    let spectrum = match state.shared.spectrum_db.lock() {
                        Ok(guard) => guard.clone(),
                        Err(_) => return,
                    };
                    draw_spectrum_line(ui, &spectrum, sr, fft_size);
                });
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        },
    )
}

// ─── Zero-copy manifold-gpu demo strip ──────────────────────────────

const GPU_DEMO_W: u32 = 256;
const GPU_DEMO_H: u32 = 24;

fn draw_gpu_demo_strip(ui: &mut egui::Ui, state: &mut EditorState) {
    // Always reserve space so the UI stays consistent; we draw a visible
    // diagnostic if init fails.
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), GPU_DEMO_H as f32),
        egui::Sense::hover(),
    );

    if state.gpu.is_none() {
        state.gpu = PluginGpuRenderer::new(GPU_DEMO_W, GPU_DEMO_H);
    }
    let Some(gpu) = state.gpu.as_mut() else {
        ui.painter().rect_filled(rect, 0.0, egui::Color32::from_rgb(120, 30, 30));
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "manifold-gpu: init failed (see Console.app)",
            egui::FontId::monospace(10.0),
            egui::Color32::WHITE,
        );
        return;
    };
    let t = state.start.elapsed().as_secs_f32();
    gpu.render_color_strip(t);

    let quad = state.quad.clone();
    let iosurface_addr = gpu.iosurface() as usize;
    let w = gpu.width();
    let h = gpu.height();

    let callback = egui::PaintCallback {
        rect,
        callback: Arc::new(egui_glow::CallbackFn::new(move |_info, painter| {
            let gl = painter.gl();
            let mut lock = quad.lock().unwrap();
            if matches!(*lock, PainterState::NotYet) {
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

    ui.painter().text(
        egui::pos2(rect.left() + 6.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "manifold-gpu (zero-copy)",
        egui::FontId::monospace(10.0),
        egui::Color32::from_rgb(255, 255, 255),
    );
}

/// Metal-side renderer. Writes into an IOSurface-backed texture; egui's GL side
/// reads from the same IOSurface via `QuadPainter`. No CPU round-trip.
struct PluginGpuRenderer {
    device: GpuDevice,
    target: IoSurfaceMtlTexture,
}

impl PluginGpuRenderer {
    fn new(width: u32, height: u32) -> Option<Self> {
        eprintln!("manifold-analyzer-gui: PluginGpuRenderer::new({width}x{height})");
        let device = GpuDevice::new();
        let target = IoSurfaceMtlTexture::new(&device, width, height)?;
        Some(Self { device, target })
    }

    fn render_color_strip(&mut self, t: f32) {
        let r = (0.5 + 0.5 * (t * 1.3).sin()) as f64;
        let g = (0.5 + 0.5 * (t * 0.9 + 2.0).sin()) as f64;
        let b = (0.5 + 0.5 * (t * 0.7 + 4.0).sin()) as f64;

        let mut enc = self.device.create_encoder("manifold-gpu demo");
        enc.clear_texture(self.target.gpu_texture(), r, g, b, 1.0);
        // Must wait for completion — the GL side samples the same IOSurface.
        enc.commit_and_wait_completed();
    }

    fn iosurface(&self) -> gpu_bridge::IOSurfaceRef {
        self.target.iosurface_raw()
    }

    fn width(&self) -> u32 {
        self.target.width
    }

    fn height(&self) -> u32 {
        self.target.height
    }
}

// ─── egui spectrum line (to be migrated to manifold-gpu later) ──────

fn draw_spectrum_line(
    ui: &mut egui::Ui,
    spectrum_db: &[f32],
    sample_rate: f32,
    fft_size: usize,
) {
    let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
    let painter = ui.painter_at(rect);

    let freq_min = 20.0_f32;
    let freq_max = (sample_rate * 0.5).max(freq_min * 2.0);
    let db_min = -90.0_f32;
    let db_max = 0.0_f32;

    let grid_color = egui::Color32::from_gray(40);
    let label_color = egui::Color32::from_gray(140);

    for &freq in &[100.0_f32, 1000.0, 10_000.0] {
        if freq <= freq_max {
            let x = freq_to_x(freq, freq_min, freq_max, rect);
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                (1.0, grid_color),
            );
            let label = if freq >= 1000.0 {
                format!("{}k", (freq / 1000.0) as i32)
            } else {
                format!("{}", freq as i32)
            };
            painter.text(
                egui::pos2(x + 4.0, rect.bottom() - 4.0),
                egui::Align2::LEFT_BOTTOM,
                label,
                egui::FontId::monospace(10.0),
                label_color,
            );
        }
    }

    for db in [-20.0_f32, -40.0, -60.0, -80.0] {
        let y = db_to_y(db, db_min, db_max, rect);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            (1.0, grid_color),
        );
        painter.text(
            egui::pos2(rect.left() + 4.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{} dB", db as i32),
            egui::FontId::monospace(10.0),
            label_color,
        );
    }

    let mut points = Vec::with_capacity(spectrum_db.len());
    for (bin, &db) in spectrum_db.iter().enumerate() {
        let freq = bin as f32 * sample_rate / fft_size as f32;
        if freq < freq_min || freq > freq_max {
            continue;
        }
        let x = freq_to_x(freq, freq_min, freq_max, rect);
        let y = db_to_y(db.clamp(db_min, db_max), db_min, db_max, rect);
        points.push(egui::pos2(x, y));
    }

    if points.len() >= 2 {
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(1.5, egui::Color32::from_rgb(100, 210, 255)),
        ));
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
