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

/// Virtual log-spaced "display bins" we resample the raw linear FFT into
/// before pushing a column. 2048 gives ~260 bins/octave over 10 Hz–25 kHz,
/// well above display pixel density so the shader can linear-interp
/// between adjacent log bins and see smooth gradients instead of the
/// raw-FFT step structure. The resampler integrates raw power across each
/// log bin's frequency span, so this is both upsampling (at low freq) and
/// anti-aliasing (at high freq).
const LOG_BINS: usize = 2048;

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
    // Log-resampler state — pre-allocated so the GUI-thread hot path is
    // allocation-free. Spectral reassignment is a scatter: for each FFT
    // bin, we compute linear power, look up its instantaneous frequency,
    // and add its power to the log bin covering that frequency.
    // `log_power_scratch` is the accumulator (cleared per frame);
    // `log_scratch` holds the final dB per log bin after log10.
    log_freq_min_ln: f32,
    log_freq_max_ln: f32,
    power_scratch: Vec<f32>,
    log_power_scratch: Vec<f32>,
    log_scratch: Vec<f32>,
}

impl SpectrumGpuRenderer {
    /// Create the renderer with a fixed-size render target. `num_bins` must
    /// match `fft_size / 2` on the audio side.
    pub fn new(device: &GpuDevice, width: u32, height: u32, num_bins: usize) -> Option<Self> {
        let target = IoSurfaceMtlTexture::new(device, width, height)?;

        let mid_buf = device.create_buffer_shared((num_bins * 4) as u64);
        let side_buf = device.create_buffer_shared((num_bins * 4) as u64);
        let history_buf =
            device.create_buffer_shared((LOG_BINS as u64) * (HISTORY_COLS as u64) * 4);

        // Shared buffers are allocated zero-initialised. For the spectrogram
        // history, 0 dB would hit the top of the colourmap (solid red) on
        // every unwritten column — fill with a low floor so unseen history
        // reads as silence (black) until real frames overwrite it.
        if let Some(ptr) = history_buf.mapped_ptr() {
            let num_elems = LOG_BINS * HISTORY_COLS as usize;
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
            log_freq_min_ln: f32::NAN,
            log_freq_max_ln: f32::NAN,
            power_scratch: vec![0.0; num_bins],
            log_power_scratch: vec![0.0; LOG_BINS],
            log_scratch: vec![-140.0; LOG_BINS],
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

    /// Append one un-averaged Mid frame to the spectrogram ring using
    /// **spectral reassignment**: for each FFT bin, we look up its
    /// instantaneous frequency (derived by the DSP from STFT phase
    /// advance between consecutive frames) and scatter-add its linear
    /// power into the log bin covering that frequency. This collapses
    /// FFT main-lobe smears (a pure tone spreads across ~1.5 bins in a
    /// windowed FFT) down to near-delta peaks — the effect is that
    /// harmonic lines read as razor-thin.
    ///
    /// The scatter's linear-split between adjacent log bins preserves
    /// sub-log-bin frequency accuracy so nearby tones don't visually
    /// snap to the same log bin.
    pub fn push_spectrogram_frame(
        &mut self,
        raw_db: &[f32],
        inst_freqs: &[f32],
        freq_min: f32,
        freq_max: f32,
    ) {
        debug_assert_eq!(raw_db.len(), self.num_bins);
        debug_assert_eq!(inst_freqs.len(), self.num_bins);

        let log_min = freq_min.ln();
        let log_max = freq_max.ln();
        if log_min != self.log_freq_min_ln || log_max != self.log_freq_max_ln {
            self.log_freq_min_ln = log_min;
            self.log_freq_max_ln = log_max;
        }

        self.reassign_to_log(raw_db, inst_freqs, freq_min, freq_max);

        self.write_col = (self.write_col + 1) % HISTORY_COLS;
        if let Some(ptr) = self.history_buf.mapped_ptr() {
            unsafe {
                let offset = self.write_col as usize * LOG_BINS * 4;
                std::ptr::copy_nonoverlapping(
                    self.log_scratch.as_ptr() as *const u8,
                    ptr.add(offset),
                    LOG_BINS * 4,
                );
            }
        }
    }

    /// Scatter-reassignment resampler. Each FFT bin contributes its linear
    /// power to one or two log bins at the bin's instantaneous frequency,
    /// not its nominal frequency. Tones whose main lobes spread across
    /// ~1.5 FFT bins all report the same instantaneous frequency, so
    /// their power sums into a single log bin = near-delta peak.
    fn reassign_to_log(
        &mut self,
        raw_db: &[f32],
        inst_freqs: &[f32],
        freq_min: f32,
        freq_max: f32,
    ) {
        let num_fft_bins = raw_db.len();

        // dB → linear power once.
        for (i, &db) in raw_db.iter().enumerate() {
            self.power_scratch[i] = 10.0_f32.powf(db * 0.1);
        }

        // Clear scatter accumulator.
        self.log_power_scratch.fill(0.0);

        let log_lo = self.log_freq_min_ln;
        let inv_log_span = 1.0 / (self.log_freq_max_ln - log_lo);
        let log_bins_f = LOG_BINS as f32;

        for bin in 0..num_fft_bins {
            let freq = inst_freqs[bin];
            if !(freq > 0.0 && freq >= freq_min && freq <= freq_max) {
                continue;
            }
            let t = (freq.ln() - log_lo) * inv_log_span;
            let log_bin_f = t * log_bins_f;
            let lo = log_bin_f as usize;
            if lo >= LOG_BINS {
                continue;
            }
            let frac = log_bin_f - lo as f32;
            let power = self.power_scratch[bin];
            // Split between adjacent log bins (sub-log-bin accuracy).
            self.log_power_scratch[lo] += power * (1.0 - frac);
            let hi = lo + 1;
            if hi < LOG_BINS {
                self.log_power_scratch[hi] += power * frac;
            }
        }

        // Accumulated linear power → dB.
        for i in 0..LOG_BINS {
            let p = self.log_power_scratch[i];
            self.log_scratch[i] = if p > 1e-20 {
                10.0 * p.log10()
            } else {
                -140.0
            };
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
            log_bins: LOG_BINS as f32,
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
