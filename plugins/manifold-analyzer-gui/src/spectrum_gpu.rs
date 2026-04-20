//! Spectrum lines + scrolling spectrogram rendered by manifold-gpu into an
//! IOSurface-backed texture.
//!
//! One shader, one draw, two regions. Top region draws Mid + Side curves;
//! bottom region samples a ring-buffer history of Mid frames for the
//! spectrogram.
//!
//! Pattern:
//! 1. WGSL fragment shader reads `mid_spectrum[]` + `side_spectrum[]` (current
//!    frame) and `history[]` (ring buffer of past Mid frames) from storage
//!    buffers, maps each pixel (x → log-freq → bin), and produces either a
//!    curve+fill composite (top) or a colourmapped history sample (bottom).
//! 2. `device.draw_fullscreen` renders into the IOSurface-backed `GpuTexture`.
//! 3. `commit_and_wait_completed` syncs; egui's PaintCallback samples the same
//!    IOSurface via GL_TEXTURE_RECTANGLE (see `gl_paint.rs`).
//!
//! The uniform + spectrum + history storage buffers are allocated once and
//! rewritten via the shared-memory mapped pointer each frame — zero audio-
//! thread cost, small memcpys on the GUI thread.

use crate::cqt::CqtTransform;
use crate::gpu_bridge::IoSurfaceMtlTexture;
use manifold_gpu::{
    GpuBinding, GpuBuffer, GpuDevice, GpuRenderPipeline, GpuTextureFormat,
};
use std::ffi::c_void;

const SHADER: &str = include_str!("../shaders/spectrum_line.wgsl");

/// Number of historical Mid frames kept for the spectrogram. One column is
/// written per FFT hop; at 8.5 ms/hop, 1024 cols ≈ 8.7 s of scroll-back,
/// enough to fill a wide window.
const HISTORY_COLS: u32 = 1024;

/// VQT parameters. Variable-Q transform: each bin's bandwidth is
/// `bandwidth(f) = α·f + γ`. At high freq `α·f` dominates (constant-Q
/// behavior, tight pitch); at low freq `γ` floors the bandwidth so bass
/// windows stay tractable instead of growing to seconds. `γ = 0` reduces
/// to classical CQT.
///
/// With `γ = 20 Hz` the longest window hits ~50 ms at the bottom of the
/// range, so an N_fft of 65 536 (≈ 1.37 s) covers everything down to
/// ~10 Hz comfortably.
const CQT_N_FFT: usize = 65_536;
const CQT_FMIN_HZ: f32 = 10.0;
const CQT_FMAX_HZ: f32 = 22_000.0;
const CQT_BINS_PER_OCTAVE: usize = 24;
/// Bandwidth floor in Hz. 0 = pure CQT (bass smears over 1 s).
/// 20 = balanced hybrid (bass transients in ~50 ms).
/// ERB-match would be ~25 Hz.
const CQT_GAMMA_HZ: f32 = 20.0;
/// Prune kernel entries below this fraction of each row's peak.
/// 0.005 keeps the main lobe plus a decibel or two of skirts; below that
/// is noise-floor material.
const CQT_THRESHOLD_REL: f32 = 0.005;
/// Samples between consecutive VQT columns. 512 at 48 kHz = 10.67 ms per
/// column, 93.75 columns per second. Well above 60 fps display rate so
/// every render frame ingests ~1.5 new columns on average.
const CQT_HOP_SAMPLES: usize = 512;

/// Noise-floor gate. Log bins whose CQT dB sits below this cut off to
/// the black floor so quiet background stays fully black instead of
/// speckling with sub-audible bin-level dithering.
const NOISE_GATE_DB: f32 = -90.0;
const FLOOR_DB: f32 = -140.0;

/// SPAN-style display options driving the fragment shader.
#[derive(Copy, Clone, Debug)]
pub struct DisplayConfig {
    pub slope_db_per_oct: f32,
    pub slope_ref_freq: f32,
    pub align_offset_db: f32,
    /// Half-bandwidth of frequency smoothing in log2(octave). Set to
    /// `1.0 / 24.0` for a 1/12-oct smoothing (±1/24 oct either side of the
    /// centre frequency). `0.0` disables smoothing.
    pub smooth_half_oct_log2: f32,
    /// Alpha of the fill below each curve. `0.0` disables fill.
    pub fill_alpha: f32,
    /// Fraction of total render-target height devoted to the spectrum curves.
    /// Remainder is spectrogram. `1.0` disables the spectrogram region.
    pub spectrum_fraction: f32,
    /// dB range that maps onto the spectrogram colourmap (independent of
    /// the spectrum curve's `db_min`/`db_max` axis). Vision 4X's Heatmap
    /// default is `-59 … 0`.
    pub spectrogram_db_min: f32,
    pub spectrogram_db_max: f32,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            slope_db_per_oct: 0.0,
            slope_ref_freq: 1000.0,
            align_offset_db: 0.0,
            smooth_half_oct_log2: 0.0,
            fill_alpha: 0.0,
            spectrum_fraction: 1.0,
            spectrogram_db_min: -59.0,
            spectrogram_db_max: 0.0,
        }
    }
}

/// Matches the `Uniforms` struct in `spectrum_line.wgsl`.
/// Total size 128 bytes, 16-byte aligned.
#[repr(C)]
#[derive(Copy, Clone)]
struct SpectrumUniforms {
    resolution: [f32; 2],
    sample_rate: f32,
    fft_size: f32,
    freq_min: f32,
    freq_max: f32,
    db_min: f32,
    db_max: f32,
    line_color: [f32; 4],
    bg_color: [f32; 4],
    side_color: [f32; 4],
    line_thickness: f32,
    slope_db_per_oct: f32,
    slope_ref_freq: f32,
    align_offset_db: f32,
    smooth_half_oct_log2: f32,
    fill_alpha: f32,
    spectrum_height: f32,
    history_cols: f32,
    write_col: f32,
    spectrogram_db_min: f32,
    spectrogram_db_max: f32,
    log_bins: f32,
}

pub struct SpectrumGpuRenderer {
    target: IoSurfaceMtlTexture,
    mid_buf: GpuBuffer,
    side_buf: GpuBuffer,
    history_buf: GpuBuffer,
    pipeline: GpuRenderPipeline,
    num_bins: usize,
    display: DisplayConfig,
    write_col: u32,
    // Spectrogram pipeline. Audio samples arrive in variable-sized chunks
    // via `ingest_samples`; we keep an N_fft-long rolling window and run
    // the CQT every CQT_HOP_SAMPLES samples. Each CQT emits one column
    // into `history_buf`, which is sized `cqt.num_bins() × HISTORY_COLS`.
    cqt: CqtTransform,
    cqt_num_bins: usize,
    // Rolling window: latest CQT_N_FFT samples, newest at `rolling_head`.
    rolling: Vec<f32>,
    rolling_head: usize,
    samples_since_last_hop: usize,
    fft_scratch_audio: Vec<f32>,
    cqt_out: Vec<f32>,
}

impl SpectrumGpuRenderer {
    /// Create the renderer with a fixed-size render target. `num_bins` is
    /// the top-curve FFT's half-size (for the averaged Mid/Side storage
    /// buffers); the spectrogram CQT has its own count derived from
    /// `sample_rate` + `CQT_FMIN_HZ/FMAX_HZ/BINS_PER_OCTAVE`.
    pub fn new(
        device: &GpuDevice,
        width: u32,
        height: u32,
        num_bins: usize,
        sample_rate: f32,
    ) -> Option<Self> {
        let target = IoSurfaceMtlTexture::new(device, width, height)?;

        let mid_buf = device.create_buffer_shared((num_bins * 4) as u64);
        let side_buf = device.create_buffer_shared((num_bins * 4) as u64);

        let fmax = CQT_FMAX_HZ.min(sample_rate * 0.5);
        let cqt = CqtTransform::new(
            sample_rate,
            CQT_N_FFT,
            CQT_FMIN_HZ,
            fmax,
            CQT_BINS_PER_OCTAVE,
            CQT_GAMMA_HZ,
            CQT_THRESHOLD_REL,
        );
        let cqt_num_bins = cqt.num_bins();

        let history_buf =
            device.create_buffer_shared((cqt_num_bins as u64) * (HISTORY_COLS as u64) * 4);

        // Shared buffers are zero-initialised. 0 dB would hit the top of
        // the colourmap (solid red) on every unwritten column — fill with
        // a low floor so unseen history reads as silence (black) until
        // real frames overwrite it.
        if let Some(ptr) = history_buf.mapped_ptr() {
            let num_elems = cqt_num_bins * HISTORY_COLS as usize;
            let slice = unsafe { std::slice::from_raw_parts_mut(ptr as *mut f32, num_elems) };
            slice.fill(-140.0);
        }
        let pipeline = device.create_render_pipeline(
            SHADER,
            "vs_main",
            "fs_main",
            GpuTextureFormat::Bgra8Unorm,
            None,
            "spectrum_line",
        );

        Some(Self {
            target,
            mid_buf,
            side_buf,
            history_buf,
            pipeline,
            num_bins,
            display: DisplayConfig::default(),
            write_col: 0,
            cqt,
            cqt_num_bins,
            rolling: vec![0.0; CQT_N_FFT],
            rolling_head: 0,
            samples_since_last_hop: 0,
            fft_scratch_audio: vec![0.0; CQT_N_FFT],
            cqt_out: vec![FLOOR_DB; cqt_num_bins],
        })
    }

    pub fn set_display(&mut self, config: DisplayConfig) {
        self.display = config;
    }

    pub fn iosurface(&self) -> *mut c_void {
        self.target.iosurface_raw()
    }

    pub fn width(&self) -> u32 {
        self.target.width
    }

    pub fn height(&self) -> u32 {
        self.target.height
    }

    /// Resize the IOSurface-backed texture to `new_w × new_h` if different
    /// from the current size. Returns `true` if a rebuild happened — the
    /// caller must invalidate the GL-side `QuadPainter` because it bound
    /// to the now-dropped IOSurface.
    pub fn ensure_size(&mut self, device: &GpuDevice, new_w: u32, new_h: u32) -> bool {
        if new_w == self.target.width && new_h == self.target.height {
            return false;
        }
        if let Some(new_target) = IoSurfaceMtlTexture::new(device, new_w, new_h) {
            self.target = new_target;
            true
        } else {
            false
        }
    }

    /// Number of log bins in the spectrogram's vertical axis (== number
    /// of CQT output bins).
    #[allow(dead_code)] // diagnostic accessor
    pub fn cqt_num_bins(&self) -> usize {
        self.cqt_num_bins
    }

    /// Consume a block of mono audio samples. Appends them to the rolling
    /// N_fft window and, every `CQT_HOP_SAMPLES`, runs one CQT and writes
    /// a column to the history buffer. Samples beyond the rolling window
    /// naturally fall off the back (ring overwrite).
    pub fn ingest_samples(&mut self, samples: &[f32]) {
        for &s in samples {
            self.rolling[self.rolling_head] = s;
            self.rolling_head = (self.rolling_head + 1) % CQT_N_FFT;
            self.samples_since_last_hop += 1;
            if self.samples_since_last_hop >= CQT_HOP_SAMPLES {
                self.samples_since_last_hop = 0;
                self.emit_column();
            }
        }
    }

    fn emit_column(&mut self) {
        // Copy the rolling ring into linear order (oldest → newest).
        // `rolling_head` is the next write position, so oldest sample is
        // at `rolling_head` and the ring reads out as
        // [head..N_fft) ++ [0..head).
        let head = self.rolling_head;
        let (tail, front) = self.rolling.split_at(head);
        self.fft_scratch_audio[..front.len()].copy_from_slice(front);
        self.fft_scratch_audio[front.len()..].copy_from_slice(tail);

        self.cqt.process(&self.fft_scratch_audio, &mut self.cqt_out);

        for v in self.cqt_out.iter_mut() {
            if *v < NOISE_GATE_DB {
                *v = FLOOR_DB;
            }
        }

        self.write_col = (self.write_col + 1) % HISTORY_COLS;
        if let Some(ptr) = self.history_buf.mapped_ptr() {
            unsafe {
                let offset = self.write_col as usize * self.cqt_num_bins * 4;
                std::ptr::copy_nonoverlapping(
                    self.cqt_out.as_ptr() as *const u8,
                    ptr.add(offset),
                    self.cqt_num_bins * 4,
                );
            }
        }
    }

    /// Render one frame. `mid_db` / `side_db` feed the Mid + Side curves
    /// (averaged); the spectrogram is drawn from history already populated
    /// via `push_spectrogram_frame`.
    pub fn render(
        &mut self,
        device: &GpuDevice,
        mid_db: &[f32],
        side_db: &[f32],
        sample_rate: f32,
        freq_min: f32,
        freq_max: f32,
        db_min: f32,
        db_max: f32,
    ) {
        debug_assert_eq!(mid_db.len(), self.num_bins);
        debug_assert_eq!(side_db.len(), self.num_bins);

        // Zero-copy upload into the shared-storage spectrum buffers.
        if let Some(ptr) = self.mid_buf.mapped_ptr() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    mid_db.as_ptr() as *const u8,
                    ptr,
                    mid_db.len() * 4,
                );
            }
        }
        if let Some(ptr) = self.side_buf.mapped_ptr() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    side_db.as_ptr() as *const u8,
                    ptr,
                    side_db.len() * 4,
                );
            }
        }

        let spectrum_fraction = self.display.spectrum_fraction.clamp(0.1, 1.0);
        let spectrum_height = (self.target.height as f32 * spectrum_fraction).round();

        let uniforms = SpectrumUniforms {
            resolution: [self.target.width as f32, self.target.height as f32],
            sample_rate,
            fft_size: (self.num_bins * 2) as f32,
            freq_min,
            freq_max,
            db_min,
            db_max,
            // Mid — bright green outline, darker green fill handled in shader via fill_alpha.
            line_color: [0.72, 0.98, 0.38, 1.0],
            bg_color: [0.031, 0.039, 0.055, 1.0], // panel bg (8,10,14)/255
            // Side — red/orange outline, same fill-alpha strategy.
            side_color: [0.95, 0.30, 0.15, 1.0],
            line_thickness: 1.2,
            slope_db_per_oct: self.display.slope_db_per_oct,
            slope_ref_freq: self.display.slope_ref_freq,
            align_offset_db: self.display.align_offset_db,
            smooth_half_oct_log2: self.display.smooth_half_oct_log2,
            fill_alpha: self.display.fill_alpha,
            spectrum_height,
            history_cols: HISTORY_COLS as f32,
            write_col: self.write_col as f32,
            spectrogram_db_min: self.display.spectrogram_db_min,
            spectrogram_db_max: self.display.spectrogram_db_max,
            log_bins: self.cqt_num_bins as f32,
        };
        let uniform_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                &uniforms as *const SpectrumUniforms as *const u8,
                std::mem::size_of::<SpectrumUniforms>(),
            )
        };

        let bindings = &[
            GpuBinding::Bytes {
                binding: 0,
                data: uniform_bytes,
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &self.mid_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &self.side_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &self.history_buf,
                offset: 0,
            },
        ];

        let mut enc = device.create_encoder("spectrum line");
        enc.draw_fullscreen(
            &self.pipeline,
            self.target.gpu_texture(),
            bindings,
            true, // clear
            true, // store
            "spectrum_line",
        );
        enc.commit_and_wait_completed();
    }
}
