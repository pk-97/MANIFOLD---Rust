//! MetalFX Spatial full-frame upscaler with RCAS sharpening.
//!
//! Two-pass pipeline:
//!   1. MetalFX Spatial: render-res → output-res (ML-based, Apple Silicon)
//!   2. RCAS: output-res sharpening pass (Robust Contrast-Adaptive Sharpening)
//!
//! MetalFX handles the upscaling better than FSR EASU; RCAS recovers the
//! edge definition that spatial upscaling softens. The combination matches
//! or exceeds FSR 1.0 quality while running faster on Apple Silicon.
//!
//! Usage:
//! ```ignore
//! let upscaler = MetalFxFullFrameUpscaler::new(device, render_w, render_h, output_w, output_h)?;
//! // each frame:
//! upscaler.upscale(&mut gpu, compositor_output_texture);
//! // read from upscaler.output.texture (at output_w × output_h)
//! ```

#[cfg(target_os = "macos")]
mod imp {
    use crate::gpu_encoder::GpuEncoder;
    use crate::render_target::RenderTarget;

    /// RCAS uniform layout. 16-byte aligned.
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct RcasUniforms {
        /// `exp2(−user_sharpness)`. Lower → stronger. Default `exp2(−0.87) ≈ 0.547`.
        sharpness: f32,
        _pad0: f32,
        _pad1: f32,
        _pad2: f32,
    }

    /// GPU full-frame upscaler: MetalFX Spatial + RCAS sharpening.
    /// Created once per (src_dims, dst_dims); call `resize()` on dimension change.
    pub struct MetalFxFullFrameUpscaler {
        scaler: manifold_gpu::metalfx::MetalFxSpatialScaler,
        /// RCAS compute pipeline (reuses the FSR1 RCAS shader).
        rcas_pipeline: manifold_gpu::GpuComputePipeline,
        rcas_sampler: manifold_gpu::GpuSampler,
        /// MetalFX output at dst_w × dst_h. Input for RCAS.
        metalfx_intermediate: RenderTarget,
        /// Final output at dst_w × dst_h (RCAS output). Blit this to IOSurface.
        pub output: RenderTarget,
        pub src_w: u32,
        pub src_h: u32,
        pub dst_w: u32,
        pub dst_h: u32,
    }

    impl MetalFxFullFrameUpscaler {
        /// Create an upscaler for the given dimensions.
        /// Returns `None` if MetalFX Spatial is not available on this device.
        pub fn new(
            device: &manifold_gpu::GpuDevice,
            src_w: u32,
            src_h: u32,
            dst_w: u32,
            dst_h: u32,
        ) -> Option<Self> {
            let fmt = manifold_gpu::GpuTextureFormat::Rgba16Float;
            let scaler = manifold_gpu::metalfx::MetalFxSpatialScaler::new(
                device.raw_device(),
                src_w, src_h, dst_w, dst_h,
                fmt,
            )?;
            let rcas_pipeline = device.create_compute_pipeline(
                include_str!("effects/shaders/fsr1_rcas_compute.wgsl"),
                "cs_main",
                "MetalFX RCAS",
            );
            let rcas_sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                address_mode_u: manifold_gpu::GpuAddressMode::ClampToEdge,
                address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
                address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
                min_filter: manifold_gpu::GpuFilterMode::Linear,
                mag_filter: manifold_gpu::GpuFilterMode::Linear,
                ..Default::default()
            });
            let metalfx_intermediate = RenderTarget::new(
                device, dst_w, dst_h, fmt, "MetalFX Intermediate",
            );
            let output = RenderTarget::new(device, dst_w, dst_h, fmt, "MetalFX+RCAS Output");
            Some(Self {
                scaler, rcas_pipeline, rcas_sampler,
                metalfx_intermediate, output,
                src_w, src_h, dst_w, dst_h,
            })
        }

        /// Returns `true` if MetalFX Spatial Scaler is supported on this device.
        pub fn is_available(device: &manifold_gpu::GpuDevice) -> bool {
            manifold_gpu::metalfx::supports_spatial_scaling(device.raw_device())
        }

        /// Upscale `source` (at src_w × src_h) → `self.output` (at dst_w × dst_h).
        ///
        /// Pass 1: MetalFX Spatial → intermediate (ends any active encoder).
        /// Pass 2: RCAS sharpening → output (new compute encoder).
        ///
        /// `sharpness_exp` = `exp2(−user_sharpness)` for RCAS. Pass 0.547 for
        /// AMD's default sharpening level (same as the FSR 1.0 fallback path).
        pub fn upscale(
            &self,
            gpu: &mut GpuEncoder,
            source: &manifold_gpu::GpuTexture,
            sharpness_exp: f32,
        ) {
            // Pass 1: MetalFX Spatial — source → metalfx_intermediate
            let cmd_buf = gpu.native_enc.raw_cmd_buf();
            self.scaler.encode(cmd_buf, source, &self.metalfx_intermediate.texture);

            // Pass 2: RCAS — intermediate → output
            let rcas_u = RcasUniforms {
                sharpness: sharpness_exp.clamp(0.01, 1.0),
                _pad0: 0.0, _pad1: 0.0, _pad2: 0.0,
            };
            gpu.native_enc.dispatch_compute(
                &self.rcas_pipeline,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&rcas_u),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: &self.metalfx_intermediate.texture,
                    },
                    manifold_gpu::GpuBinding::Sampler {
                        binding: 2,
                        sampler: &self.rcas_sampler,
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 3,
                        texture: &self.output.texture,
                    },
                ],
                [self.dst_w.div_ceil(16), self.dst_h.div_ceil(16), 1],
                "MetalFX RCAS",
            );
        }

        /// Resize both the scaler and internal textures when dimensions change.
        /// Returns `false` if the new scaler could not be created (MetalFX unavailable).
        pub fn resize(
            &mut self,
            device: &manifold_gpu::GpuDevice,
            src_w: u32,
            src_h: u32,
            dst_w: u32,
            dst_h: u32,
        ) -> bool {
            let fmt = manifold_gpu::GpuTextureFormat::Rgba16Float;
            let Some(scaler) = manifold_gpu::metalfx::MetalFxSpatialScaler::new(
                device.raw_device(),
                src_w, src_h, dst_w, dst_h,
                fmt,
            ) else {
                return false;
            };
            self.scaler = scaler;
            self.src_w = src_w;
            self.src_h = src_h;
            self.dst_w = dst_w;
            self.dst_h = dst_h;
            self.metalfx_intermediate.resize(device, dst_w, dst_h);
            self.output.resize(device, dst_w, dst_h);
            true
        }
    }
}

#[cfg(target_os = "macos")]
pub use imp::MetalFxFullFrameUpscaler;
