//! GPU waterfall renderer for VQT magnitude columns.
//!
//! Consumes per-hop magnitude columns (from [`crate::cqt::CqtTransform`]) into a
//! CPU ring, and renders a scrolling, colour-mapped spectrogram into a caller-
//! owned [`GpuTexture`] with one fullscreen pass. Purpose-built for Manifold's
//! Audio Setup scope — no egui/GL coupling. The dB conversion, colour ramp, and
//! log-frequency mapping all live in `shaders/spectrogram.wgsl`.
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

/// Uniform params for the shader. `#[repr(C)]`, 16-byte aligned (two `vec4`-
/// sized rows) per the GPU uniform-alignment convention.
#[repr(C)]
#[derive(Clone, Copy)]
struct Params {
    num_bins: u32,
    history_len: u32,
    write_index: u32,
    _pad0: u32,
    db_min: f32,
    db_max: f32,
    band_lo_y: f32,
    band_hi_y: f32,
}

/// Scrolling spectrogram renderer. One per visible scope.
pub struct Spectrogram {
    num_bins: usize,
    history_len: usize,
    /// `history_len * num_bins` magnitudes; a ring of columns. `head` is the
    /// next column to overwrite (also the shader's `write_index`).
    ring: Vec<f32>,
    head: usize,
    bufs: Vec<GpuBuffer>,
    buf_frame: usize,
    pipeline: GpuRenderPipeline,
    db_min: f32,
    db_max: f32,
}

impl Spectrogram {
    /// Create a renderer for `num_bins`-tall columns and `history_len` columns
    /// of scroll-back. `color_format` must match the texture passed to
    /// [`render`](Self::render). `db_min`/`db_max` set the magnitude→colour
    /// dynamic range (e.g. −72 dB → 0 dB).
    pub fn new(
        device: &GpuDevice,
        num_bins: usize,
        history_len: usize,
        color_format: GpuTextureFormat,
        db_min: f32,
        db_max: f32,
    ) -> Self {
        let elems = num_bins * history_len;
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
            history_len,
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

    /// Append one magnitude column (advancing the ring). Extra values past
    /// `num_bins` are ignored; a short column zero-pads the remainder.
    pub fn push_column(&mut self, magnitudes: &[f32]) {
        let base = self.head * self.num_bins;
        let dst = &mut self.ring[base..base + self.num_bins];
        let n = magnitudes.len().min(self.num_bins);
        dst[..n].copy_from_slice(&magnitudes[..n]);
        for v in &mut dst[n..] {
            *v = 0.0;
        }
        self.head = (self.head + 1) % self.history_len;
    }

    /// Render the current history into `target` (cleared first). One fullscreen
    /// pass sampling the rotating buffer this frame writes. `band_ys` are two
    /// band-divider positions, normalised 0..1 from the bottom (low freq);
    /// negative disables a line.
    pub fn render(&mut self, encoder: &mut GpuEncoder, target: &GpuTexture, band_ys: [f32; 2]) {
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
            history_len: self.history_len as u32,
            write_index: self.head as u32,
            _pad0: 0,
            db_min: self.db_min,
            db_max: self.db_max,
            band_lo_y: band_ys[0],
            band_hi_y: band_ys[1],
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
