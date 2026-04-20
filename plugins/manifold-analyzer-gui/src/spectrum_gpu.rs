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
//! CQT spectrogram columns are produced off-thread by `spectrum_worker.rs`;
//! the renderer just writes incoming `CqtColumnMsg`s into the history
//! storage buffer at the specified column index.

use crate::gpu_bridge::IoSurfaceMtlTexture;
use crate::spectrum_worker::{CqtBuildParams, CqtColumnMsg};
use manifold_gpu::{
    GpuBinding, GpuBuffer, GpuDevice, GpuRenderPipeline, GpuTextureFormat,
};
use std::ffi::c_void;

const SHADER: &str = include_str!("../shaders/spectrum_line.wgsl");

/// Number of historical Mid frames kept for the spectrogram. One column is
/// written per VQT hop; at 5.33 ms/hop, 2048 cols ≈ 10.9 s of scroll-back,
/// enough to fill a wide window.
pub const HISTORY_COLS: u32 = 2048;

/// VQT parameters. See `spectrum_worker::CqtBuildParams` for the runtime
/// struct consumed by the worker. Constants live here because the shader
/// also needs fmin + bpo for its log-y mapping.
pub const CQT_N_FFT: usize = 65_536;
pub const CQT_FMIN_HZ: f32 = 10.0;
pub const CQT_FMAX_HZ: f32 = 22_000.0;
pub const CQT_BINS_PER_OCTAVE: usize = 24;
const CQT_GAMMA_LO_HZ: f32 = 10.0;
const CQT_GAMMA_HI_HZ: f32 = 20.0;
const CQT_GAMMA_TRANSITION_HZ: f32 = 200.0;
const CQT_THRESHOLD_REL: f32 = 1e-4;
pub const CQT_HOP_SAMPLES: usize = 256;
const CQT_MIN_KERNEL_LEN: usize = 4 * CQT_HOP_SAMPLES;
const CQT_CAUSAL_WINDOW: bool = false;

const FLOOR_DB: f32 = -140.0;

/// Build params used to spawn the CQT worker. Derived from this file's
/// constants + the current sample rate so the worker and renderer agree on
/// layout.
pub fn cqt_build_params(sample_rate: f32) -> CqtBuildParams {
    let fmax = CQT_FMAX_HZ.min(sample_rate * 0.5);
    CqtBuildParams {
        n_fft: CQT_N_FFT,
        fmin: CQT_FMIN_HZ,
        fmax,
        bpo: CQT_BINS_PER_OCTAVE,
        gamma_lo: CQT_GAMMA_LO_HZ,
        gamma_hi: CQT_GAMMA_HI_HZ,
        gamma_transition: CQT_GAMMA_TRANSITION_HZ,
        min_kernel_len: CQT_MIN_KERNEL_LEN,
        causal: CQT_CAUSAL_WINDOW,
        threshold_rel: CQT_THRESHOLD_REL,
        hop_samples: CQT_HOP_SAMPLES,
        history_cols: HISTORY_COLS,
    }
}

/// Length of the weighting LUT uploaded to the shader. 1024 samples across
/// `[freq_min, freq_max]` in log-space is smoother than the display pixel
/// density even at 4K widths, so sampling is effectively exact.
pub const WEIGHTING_LUT_SIZE: usize = 1024;

/// SPAN-style display options driving the fragment shader.
#[derive(Copy, Clone, Debug)]
pub struct DisplayConfig {
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
    /// Whether the spectrogram is being drawn in BPM-locked "sync" mode.
    /// Set on the renderer every frame from the GUI's current param state
    /// so the shader picks the right x-axis mapping (pinned-to-grid vs
    /// scrolling). Doesn't affect CQT processing itself — that's the
    /// worker's WorkerConfig.
    pub sync_mode: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            smooth_half_oct_log2: 0.0,
            fill_alpha: 0.0,
            spectrum_fraction: 1.0,
            spectrogram_db_min: -59.0,
            spectrogram_db_max: 0.0,
            sync_mode: false,
        }
    }
}

/// Matches the `Uniforms` struct in `spectrum_line.wgsl`.
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
    smooth_half_oct_log2: f32,
    fill_alpha: f32,
    spectrum_height: f32,
    history_cols: f32,
    write_col: f32,
    spectrogram_db_min: f32,
    spectrogram_db_max: f32,
    log_bins: f32,
    sync_mode: f32,
    cqt_fmin_hz: f32,
    cqt_bins_per_octave: f32,
}

pub struct SpectrumGpuRenderer {
    target: IoSurfaceMtlTexture,
    mid_buf: GpuBuffer,
    side_buf: GpuBuffer,
    history_buf: GpuBuffer,
    /// Pre-computed weighting curve uploaded from CPU when the weighting
    /// mode or visible freq range changes. See `WEIGHTING_LUT_SIZE`.
    weighting_lut_buf: GpuBuffer,
    pipeline: GpuRenderPipeline,
    num_bins: usize,
    cqt_num_bins: usize,
    display: DisplayConfig,
    /// Last column index written by `apply_column`. Used by the shader as
    /// the newest-pixel anchor in free-scroll mode and as the wrap point
    /// in sync mode.
    write_col: u32,
}

impl SpectrumGpuRenderer {
    pub fn new(
        device: &GpuDevice,
        width: u32,
        height: u32,
        num_bins: usize,
        cqt_num_bins: usize,
    ) -> Option<Self> {
        let target = IoSurfaceMtlTexture::new(device, width, height)?;
        let mid_buf = device.create_buffer_shared((num_bins * 4) as u64);
        let side_buf = device.create_buffer_shared((num_bins * 4) as u64);
        let history_buf =
            device.create_buffer_shared((cqt_num_bins as u64) * (HISTORY_COLS as u64) * 4);

        // Shared buffers are zero-initialised. 0 dB would hit the top of the
        // colourmap (solid red) on every unwritten column — fill with a low
        // floor so unseen history reads as silence (black) until real frames
        // overwrite it.
        if let Some(ptr) = history_buf.mapped_ptr() {
            let num_elems = cqt_num_bins * HISTORY_COLS as usize;
            let slice = unsafe { std::slice::from_raw_parts_mut(ptr as *mut f32, num_elems) };
            slice.fill(FLOOR_DB);
        }

        let weighting_lut_buf =
            device.create_buffer_shared((WEIGHTING_LUT_SIZE as u64) * 4);
        // Zero-init is already "flat weighting" (no tilt applied), so the
        // shader renders sensibly on the first frame before the GUI has
        // uploaded a mode-specific LUT.

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
            weighting_lut_buf,
            pipeline,
            num_bins,
            cqt_num_bins,
            display: DisplayConfig::default(),
            // Start one before index 0 so the first free-scroll column
            // written (prev+1 wraps to 0) lands at col 0 rather than col 1.
            write_col: HISTORY_COLS - 1,
        })
    }

    /// Upload a new weighting curve. `lut` must be `WEIGHTING_LUT_SIZE`
    /// samples spanning the current displayed freq range, with any
    /// alignment-offset bias already baked in.
    pub fn set_weighting_lut(&mut self, lut: &[f32]) {
        debug_assert_eq!(lut.len(), WEIGHTING_LUT_SIZE);
        if let Some(ptr) = self.weighting_lut_buf.mapped_ptr() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    lut.as_ptr() as *const u8,
                    ptr,
                    lut.len() * 4,
                );
            }
        }
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
    /// caller must invalidate the GL-side `QuadPainter` because it bound to
    /// the now-dropped IOSurface.
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

    /// Apply one CQT column from the worker: optionally clear the
    /// spectrogram history first (tempo/sync change), then write the
    /// column data into the history storage buffer. Updates `write_col`
    /// so the shader tracks the newest column.
    pub fn apply_column(&mut self, msg: &CqtColumnMsg) {
        debug_assert_eq!(msg.data.len(), self.cqt_num_bins);
        if msg.clear_history_before {
            self.clear_history();
        }
        if let Some(ptr) = self.history_buf.mapped_ptr() {
            let stride = self.cqt_num_bins * 4;
            let col = msg.col_idx as usize;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    msg.data.as_ptr() as *const u8,
                    ptr.add(col * stride),
                    stride,
                );
            }
        }
        self.write_col = msg.col_idx;
    }

    fn clear_history(&mut self) {
        if let Some(ptr) = self.history_buf.mapped_ptr() {
            let num_elems = self.cqt_num_bins * HISTORY_COLS as usize;
            let slice = unsafe { std::slice::from_raw_parts_mut(ptr as *mut f32, num_elems) };
            slice.fill(FLOOR_DB);
        }
    }

    /// Number of log bins in the spectrogram's vertical axis (== number of
    /// CQT output bins). Useful for diagnostics.
    #[allow(dead_code)]
    pub fn cqt_num_bins(&self) -> usize {
        self.cqt_num_bins
    }

    /// Render one frame. `mid_db` / `side_db` feed the Mid + Side curves
    /// (averaged); the spectrogram is drawn from `history_buf` already
    /// populated via `apply_column`.
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
            line_color: [0.72, 0.98, 0.38, 1.0],
            bg_color: [0.031, 0.039, 0.055, 1.0],
            side_color: [0.95, 0.30, 0.15, 1.0],
            line_thickness: 1.2,
            smooth_half_oct_log2: self.display.smooth_half_oct_log2,
            fill_alpha: self.display.fill_alpha,
            spectrum_height,
            history_cols: HISTORY_COLS as f32,
            write_col: self.write_col as f32,
            spectrogram_db_min: self.display.spectrogram_db_min,
            spectrogram_db_max: self.display.spectrogram_db_max,
            log_bins: self.cqt_num_bins as f32,
            sync_mode: if self.display.sync_mode { 1.0 } else { 0.0 },
            cqt_fmin_hz: CQT_FMIN_HZ,
            cqt_bins_per_octave: CQT_BINS_PER_OCTAVE as f32,
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
            GpuBinding::Buffer {
                binding: 4,
                buffer: &self.weighting_lut_buf,
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
