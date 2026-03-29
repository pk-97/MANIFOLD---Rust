//! FSR 1.0 — FidelityFX Super Resolution spatial upscaler.
//!
//! Two compute passes:
//!   1. EASU — Edge-Adaptive Spatial Upsampling: maps render-res → output-res
//!      using direction-adaptive Catmull-Rom reconstruction.
//!   2. RCAS — Robust Contrast-Adaptive Sharpening: post-process sharpening
//!      with noise adaptation and anti-ringing.
//!
//! Usage:
//! ```ignore
//! let fsr = Fsr1Upscaler::new(device, render_w, render_h, output_w, output_h);
//! // each frame:
//! fsr.upscale(gpu, compositor_output_texture);
//! // read from fsr.output.texture (at output_w × output_h)
//! ```

use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;

/// Uniform layout for the EASU pass. 32 bytes (two vec4 rows). 16-byte aligned.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EasuUniforms {
    scale_x: f32,    // srcW / dstW
    scale_y: f32,    // srcH / dstH
    bias_x: f32,     // 0.5 * srcW/dstW − 0.5
    bias_y: f32,     // 0.5 * srcH/dstH − 0.5
    inv_src_w: f32,  // 1.0 / srcW
    inv_src_h: f32,  // 1.0 / srcH
    _pad0: f32,
    _pad1: f32,
}

/// Uniform layout for the RCAS pass. 16 bytes (one vec4 row). 16-byte aligned.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RcasUniforms {
    /// `exp2(−user_sharpness)` where `user_sharpness ∈ [0.1, 2.0]`.
    /// Lower → stronger sharpening. Default `exp2(−0.87) ≈ 0.547`.
    sharpness: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// GPU pipeline for FSR 1.0 spatial upscaling (EASU + RCAS).
pub struct Fsr1Upscaler {
    easu_pipeline: manifold_gpu::GpuComputePipeline,
    rcas_pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,
    /// EASU output at output_w × output_h. RCAS reads from this.
    easu_output: RenderTarget,
    /// RCAS output at output_w × output_h. Read by the blit to IOSurface.
    pub output: RenderTarget,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
}

impl Fsr1Upscaler {
    pub fn new(
        device: &manifold_gpu::GpuDevice,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> Self {
        let fmt = manifold_gpu::GpuTextureFormat::Rgba16Float;
        let easu_pipeline = device.create_compute_pipeline(
            include_str!("effects/shaders/fsr1_easu_compute.wgsl"),
            "cs_main",
            "FSR1 EASU",
        );
        let rcas_pipeline = device.create_compute_pipeline(
            include_str!("effects/shaders/fsr1_rcas_compute.wgsl"),
            "cs_main",
            "FSR1 RCAS",
        );
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            address_mode_u: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            ..Default::default()
        });
        let easu_output = RenderTarget::new(device, dst_w, dst_h, fmt, "FSR1 EASU Output");
        let output      = RenderTarget::new(device, dst_w, dst_h, fmt, "FSR1 RCAS Output");

        Self {
            easu_pipeline,
            rcas_pipeline,
            sampler,
            easu_output,
            output,
            src_w,
            src_h,
            dst_w,
            dst_h,
        }
    }

    /// Upscale `source` (at src_w × src_h) → `self.output` (at dst_w × dst_h).
    ///
    /// `sharpness_exp` = `exp2(−user_sharpness)` for the RCAS pass, typically
    /// computed once per settings change. Pass 0.547 for AMD's default level.
    pub fn upscale(
        &self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        sharpness_exp: f32,
    ) {
        let easu_u = EasuUniforms {
            scale_x:   self.src_w as f32 / self.dst_w as f32,
            scale_y:   self.src_h as f32 / self.dst_h as f32,
            bias_x:    0.5 * self.src_w as f32 / self.dst_w as f32 - 0.5,
            bias_y:    0.5 * self.src_h as f32 / self.dst_h as f32 - 0.5,
            inv_src_w: 1.0 / self.src_w as f32,
            inv_src_h: 1.0 / self.src_h as f32,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        // Pass 1: EASU — source (render-res) → easu_output (output-res)
        gpu.native_enc.dispatch_compute(
            &self.easu_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&easu_u),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: source,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: &self.easu_output.texture,
                },
            ],
            [
                self.dst_w.div_ceil(16),
                self.dst_h.div_ceil(16),
                1,
            ],
            "FSR1 EASU",
        );

        let rcas_u = RcasUniforms {
            sharpness: sharpness_exp.clamp(0.01, 1.0),
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        // Pass 2: RCAS — easu_output → output (both at output-res)
        gpu.native_enc.dispatch_compute(
            &self.rcas_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&rcas_u),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: &self.easu_output.texture,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: &self.output.texture,
                },
            ],
            [
                self.dst_w.div_ceil(16),
                self.dst_h.div_ceil(16),
                1,
            ],
            "FSR1 RCAS",
        );
    }

    /// Resize both internal textures when src or dst dimensions change.
    pub fn resize(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) {
        self.src_w = src_w;
        self.src_h = src_h;
        self.dst_w = dst_w;
        self.dst_h = dst_h;
        self.easu_output.resize(device, dst_w, dst_h);
        self.output.resize(device, dst_w, dst_h);
    }
}
