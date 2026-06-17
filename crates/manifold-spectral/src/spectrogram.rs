//! GPU sweep-fill renderer for VQT magnitude columns.
//!
//! Consumes per-hop magnitude columns (from [`crate::cqt::CqtTransform`]) into a
//! CPU ring whose width equals the on-screen pixel width, and paints them into a
//! caller-owned [`GpuTexture`] with one fullscreen pass. Oscilloscope-style: the
//! image is stationary and a write head sweeps left→right, overwriting the
//! oldest column in place and wrapping at the right edge — so 1 column maps to
//! exactly 1 pixel (no horizontal resampling, no scrolling judder). The dB
//! conversion, colour ramp, and log-frequency mapping all live in
//! `shaders/spectrogram.wgsl`. Purpose-built for Manifold's Audio Setup scope —
//! no egui/GL coupling.
//!
//! Race-free without locks: the column history is uploaded into one of three
//! rotating GPU buffers per render, so the CPU never writes a buffer the GPU is
//! still reading from a prior in-flight frame.

use manifold_gpu::{
    GpuBinding, GpuBuffer, GpuDevice, GpuEncoder, GpuRenderPipeline, GpuTexture, GpuTextureFormat,
};

const SHADER: &str = include_str!("shaders/spectrogram.wgsl");

/// Rotating GPU buffer count — matches the typical in-flight depth so a buffer
/// is never written while a prior frame's GPU read is outstanding.
const BUFFER_ROTATION: usize = 3;

/// Pink-noise spectral tilt (dB/octave) applied to the colourmap, matching the
/// Analyzer VST's default weighting: pink noise reads as a flat field, so a
/// real-world mix isn't dominated by its bass energy. Auto-centred over the
/// displayed range (mean 0) so overall brightness is preserved — see the
/// shader. `0.0` would be the raw "Flat" look (bass blows out to white).
const PINK_SLOPE_DB_PER_OCT: f32 = 3.0;

/// Uniform params for the shader. `#[repr(C)]`, 16-byte aligned (two `vec4`-
/// sized rows) per the GPU uniform-alignment convention.
#[repr(C)]
#[derive(Clone, Copy)]
struct Params {
    num_bins: u32,
    num_cols: u32,
    write_index: u32,
    _pad0: u32,
    db_min: f32,
    db_max: f32,
    band_lo_y: f32,
    band_hi_y: f32,
    /// Pink tilt slope (dB/octave) and the displayed range's octave span
    /// `log2(fmax/fmin)`; together they give the per-bin weighting in the
    /// shader. Slope 0 disables the tilt.
    tilt_slope: f32,
    freq_log_ratio: f32,
    /// Cursor frequency line position (uv.y, 0 top → 1 bottom); negative hides
    /// it. Drawn as a faint horizontal line so the hover readout has a locator.
    cursor_y: f32,
    _pad1: f32,
}

/// Sweep-fill spectrogram renderer. One per visible scope; sized to the scope's
/// pixel width so each column owns one pixel column.
pub struct Spectrogram {
    num_bins: usize,
    /// Column count = on-screen pixel width. One pixel column per ring column.
    num_cols: usize,
    /// `num_cols * num_bins` magnitudes; a ring of columns. `head` is the next
    /// column to overwrite (also the shader's `write_index` — the sweep line).
    ring: Vec<f32>,
    head: usize,
    bufs: Vec<GpuBuffer>,
    buf_frame: usize,
    pipeline: GpuRenderPipeline,
    db_min: f32,
    db_max: f32,
}

impl Spectrogram {
    /// Create a renderer for `num_bins`-tall columns across `num_cols` pixel
    /// columns (pass the scope's physical-pixel width). `color_format` must
    /// match the texture passed to [`render`](Self::render). `db_min`/`db_max`
    /// set the magnitude→colour dynamic range (e.g. −59 dB → 0 dB).
    pub fn new(
        device: &GpuDevice,
        num_bins: usize,
        num_cols: usize,
        color_format: GpuTextureFormat,
        db_min: f32,
        db_max: f32,
    ) -> Self {
        let elems = num_bins * num_cols;
        let bytes = (elems * std::mem::size_of::<f32>()) as u64;
        let bufs = (0..BUFFER_ROTATION)
            .map(|_| {
                let b = device.create_buffer_shared(bytes.max(4));
                b.zero_fill();
                b
            })
            .collect();
        let pipeline = device.create_render_pipeline(
            SHADER,
            "vs_main",
            "fs_main",
            color_format,
            None,
            "Spectrogram",
        );
        Self {
            num_bins,
            num_cols,
            ring: vec![0.0; elems],
            head: 0,
            bufs,
            buf_frame: 0,
            pipeline,
            db_min,
            db_max,
        }
    }

    pub fn num_bins(&self) -> usize {
        self.num_bins
    }

    /// On-screen column count this renderer was built for (== pixel width).
    pub fn num_cols(&self) -> usize {
        self.num_cols
    }

    /// Raw dB at normalised scope position (`ux` 0→1 left→right, `uy` 0→1
    /// top→bottom) — the same column/bin the shader paints, with the same
    /// power-domain 2-tap bin interpolation but WITHOUT the pink tilt, so the
    /// hover readout shows true level. Returns a deep floor for an empty cell.
    pub fn sample_db(&self, ux: f32, uy: f32) -> f32 {
        if self.num_bins == 0 || self.num_cols == 0 {
            return self.db_min;
        }
        let col = ((ux.clamp(0.0, 1.0) * self.num_cols as f32) as usize).min(self.num_cols - 1);
        let top = (self.num_bins - 1) as f32;
        let log_bin_f = ((1.0 - uy).clamp(0.0, 1.0) * top).clamp(0.0, top);
        let lo = log_bin_f.floor() as usize;
        let hi = (lo + 1).min(self.num_bins - 1);
        let frac = log_bin_f - lo as f32;
        let base = col * self.num_bins;
        let m_lo = self.ring[base + lo];
        let m_hi = self.ring[base + hi];
        let power = m_lo * m_lo * (1.0 - frac) + m_hi * m_hi * frac;
        10.0 * (power + 1e-18).log10()
    }

    /// Pink-weighted dB at the cursor: [`sample_db`](Self::sample_db) plus the
    /// exact tilt the shader applies to the colour, so the hover readout matches
    /// what's drawn under the cursor. `freq_log_ratio` is `log2(fmax/fmin)` of
    /// the displayed range (0 → no tilt, raw level). Mirrors the shader's
    /// `tilt = slope · freq_log_ratio · (0.5 − uv.y)`.
    pub fn sample_db_weighted(&self, ux: f32, uy: f32, freq_log_ratio: f32) -> f32 {
        let raw = self.sample_db(ux, uy);
        if freq_log_ratio > 0.0 {
            raw + PINK_SLOPE_DB_PER_OCT * freq_log_ratio * (0.5 - uy.clamp(0.0, 1.0))
        } else {
            raw
        }
    }

    /// Append one magnitude column at the sweep head (advancing it). Extra
    /// values past `num_bins` are ignored; a short column zero-pads the
    /// remainder. The head wraps at the right edge back to the left.
    pub fn push_column(&mut self, magnitudes: &[f32]) {
        let base = self.head * self.num_bins;
        let dst = &mut self.ring[base..base + self.num_bins];
        let n = magnitudes.len().min(self.num_bins);
        dst[..n].copy_from_slice(&magnitudes[..n]);
        for v in &mut dst[n..] {
            *v = 0.0;
        }
        self.head = (self.head + 1) % self.num_cols;
    }

    /// Render the current history into `target` (cleared first). One fullscreen
    /// pass sampling the rotating buffer this frame writes. `band_ys` are two
    /// band-divider positions, normalised 0..1 from the bottom (low freq);
    /// negative disables a line. `freq_log_ratio` is `log2(fmax/fmin)` of the
    /// displayed range — the octave span the pink tilt is centred and scaled
    /// over; pass `0.0` to disable the tilt (Flat look). `cursor_y` draws a
    /// faint horizontal locator line (uv.y, 0 top → 1 bottom); negative hides it.
    pub fn render(
        &mut self,
        encoder: &mut GpuEncoder,
        target: &GpuTexture,
        band_ys: [f32; 2],
        freq_log_ratio: f32,
        cursor_y: f32,
    ) {
        let buf = &self.bufs[self.buf_frame % BUFFER_ROTATION];
        self.buf_frame += 1;

        // SAFETY: shared buffer; `ring` is exactly the buffer's length and this
        // buffer isn't read by an in-flight frame (rotation guarantees it).
        unsafe {
            let bytes = std::slice::from_raw_parts(
                self.ring.as_ptr() as *const u8,
                std::mem::size_of_val(self.ring.as_slice()),
            );
            buf.write(0, bytes);
        }

        let params = Params {
            num_bins: self.num_bins as u32,
            num_cols: self.num_cols as u32,
            write_index: self.head as u32,
            _pad0: 0,
            db_min: self.db_min,
            db_max: self.db_max,
            band_lo_y: band_ys[0],
            band_hi_y: band_ys[1],
            tilt_slope: if freq_log_ratio > 0.0 { PINK_SLOPE_DB_PER_OCT } else { 0.0 },
            freq_log_ratio,
            cursor_y,
            _pad1: 0.0,
        };
        // SAFETY: `Params` is `#[repr(C)]` plain-old-data.
        let param_bytes = unsafe {
            std::slice::from_raw_parts(
                &params as *const Params as *const u8,
                std::mem::size_of::<Params>(),
            )
        };

        encoder.draw_fullscreen(
            &self.pipeline,
            target,
            &[
                GpuBinding::Buffer { binding: 0, buffer: buf, offset: 0 },
                GpuBinding::Bytes { binding: 1, data: param_bytes },
            ],
            true, // clear
            true, // store
            "Spectrogram",
        );
    }
}
