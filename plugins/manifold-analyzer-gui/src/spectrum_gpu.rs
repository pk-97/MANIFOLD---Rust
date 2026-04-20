//! Spectrum line rendered by manifold-gpu into an IOSurface-backed texture.
//!
//! Pattern:
//! 1. WGSL fragment shader reads `spectrum_db[]` from a storage buffer, maps
//!    each pixel (x → log-freq → bin, y → dB), anti-aliases around the line.
//! 2. `device.draw_fullscreen` renders into the IOSurface-backed `GpuTexture`.
//! 3. `commit_and_wait_completed` syncs; egui's PaintCallback samples the same
//!    IOSurface via GL_TEXTURE_RECTANGLE (see `gl_paint.rs`).
//!
//! The uniform + spectrum storage buffer are allocated once and rewritten via
//! the shared-memory mapped pointer each frame — zero audio-thread cost, one
//! ~8KB memcpy on the GUI thread.

use crate::gpu_bridge::IoSurfaceMtlTexture;
use manifold_gpu::{
    GpuBinding, GpuBuffer, GpuDevice, GpuRenderPipeline, GpuTextureFormat,
};
use std::ffi::c_void;

const SHADER: &str = include_str!("../shaders/spectrum_line.wgsl");

/// Matches the `Uniforms` struct in `spectrum_line.wgsl`.
/// Total size 80 bytes, 16-byte aligned (vec4 members force 16-byte struct alignment).
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
    line_thickness: f32,
    _pad: [f32; 3],
}

pub struct SpectrumGpuRenderer {
    target: IoSurfaceMtlTexture,
    spectrum_buf: GpuBuffer,
    pipeline: GpuRenderPipeline,
    num_bins: usize,
}

impl SpectrumGpuRenderer {
    /// Create the renderer with a fixed-size render target. `num_bins` must
    /// match `fft_size / 2` on the audio side.
    pub fn new(device: &GpuDevice, width: u32, height: u32, num_bins: usize) -> Option<Self> {
        let target = IoSurfaceMtlTexture::new(device, width, height)?;

        let spectrum_buf = device.create_buffer_shared((num_bins * 4) as u64);
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
            spectrum_buf,
            pipeline,
            num_bins,
        })
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

    /// Render one frame. `spectrum_db` length must equal `num_bins`.
    pub fn render(
        &mut self,
        device: &GpuDevice,
        spectrum_db: &[f32],
        sample_rate: f32,
        freq_min: f32,
        freq_max: f32,
        db_min: f32,
        db_max: f32,
    ) {
        debug_assert_eq!(spectrum_db.len(), self.num_bins);

        // Zero-copy upload into the shared-storage spectrum buffer.
        if let Some(ptr) = self.spectrum_buf.mapped_ptr() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    spectrum_db.as_ptr() as *const u8,
                    ptr,
                    spectrum_db.len() * 4,
                );
            }
        }

        let uniforms = SpectrumUniforms {
            resolution: [self.target.width as f32, self.target.height as f32],
            sample_rate,
            fft_size: (self.num_bins * 2) as f32,
            freq_min,
            freq_max,
            db_min,
            db_max,
            line_color: [0.39, 0.82, 1.0, 1.0], // matches previous egui cyan
            bg_color: [0.031, 0.039, 0.055, 1.0], // matches panel bg (8,10,14)/255
            line_thickness: 1.5,
            _pad: [0.0; 3],
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
                buffer: &self.spectrum_buf,
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
