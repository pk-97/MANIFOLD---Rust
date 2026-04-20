//! egui-based GUI for the Manifold Analyzer plugin.
//!
//! Renders the live mono spectrum as a log-frequency line plot.
//!
//! # TODO(manifold-gpu-migration)
//!
//! This module uses egui for **both** chrome (text, labels, grid) **and** the
//! spectrum line itself. The long-term plan is to migrate every visual
//! (spectrum line, spectrogram, difference heatmap, reference overlay) to
//! `manifold-gpu` via egui's custom paint callback, keeping egui only for
//! text/buttons/sliders/layout/input. See MEMORY: `project_analyzer_plugin.md`.
//!
//! # Audio thread ↔ GUI thread
//!
//! The audio thread writes into `AnalyzerGuiShared::spectrum_db` via
//! `try_lock` — if the GUI happens to be reading, that update is dropped
//! (fine for visualization). The GUI thread uses a blocking `lock` but
//! holds it only for the duration of one spectrum copy.

use manifold_analyzer_dsp::MIN_DB;
use nih_plug::prelude::*;
use nih_plug_egui::{EguiState, create_egui_editor, egui};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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

pub fn create_editor(
    egui_state: Arc<EguiState>,
    shared: Arc<AnalyzerGuiShared>,
) -> Option<Box<dyn Editor>> {
    create_egui_editor(
        egui_state,
        shared,
        |_, _| {},
        |ctx, _setter, shared| {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(8, 10, 14)))
                .show(ctx, |ui| {
                    let sr = shared.sample_rate();
                    let fft_size = shared.fft_size();
                    let spectrum = match shared.spectrum_db.lock() {
                        Ok(guard) => guard.clone(),
                        Err(_) => return,
                    };
                    draw_spectrum_line(ui, &spectrum, sr, fft_size);
                });
            // Drive repaint at ~60Hz — egui only repaints on input by default.
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        },
    )
}

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

    // Vertical grid at decade boundaries + common reference frequencies.
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

    // Horizontal dB grid.
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

    // Spectrum line — log frequency, linear dB.
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
