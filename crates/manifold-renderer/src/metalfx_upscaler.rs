//! MetalFX Spatial full-frame upscaler.
//!
//! Wraps `manifold_gpu::metalfx::MetalFxSpatialScaler` for compositor-output
//! upscaling (render-res → output-res). This is distinct from the per-generator
//! upscaling in `GeneratorRenderer` — this operates on the final composited frame.
//!
//! MetalFX Spatial uses Apple's ML-based perceptual upscaling algorithm tuned for
//! Apple Silicon. It produces better results than FSR 1.0 and runs faster (driver-
//! integrated, AMX/ANE acceleration on Apple Silicon). Used automatically when
//! available (macOS 13+); FSR 1.0 is the fallback.
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

    /// GPU full-frame upscaler using MetalFX Spatial Scaler.
    /// Created once per (src_dims, dst_dims); call `resize()` on dimension change.
    pub struct MetalFxFullFrameUpscaler {
        scaler: manifold_gpu::metalfx::MetalFxSpatialScaler,
        /// Output at dst_w × dst_h. Blit this to IOSurface after upscaling.
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
            let output = RenderTarget::new(device, dst_w, dst_h, fmt, "MetalFX Full Frame Output");
            Some(Self { scaler, output, src_w, src_h, dst_w, dst_h })
        }

        /// Returns `true` if MetalFX Spatial Scaler is supported on this device.
        pub fn is_available(device: &manifold_gpu::GpuDevice) -> bool {
            manifold_gpu::metalfx::supports_spatial_scaling(device.raw_device())
        }

        /// Upscale `source` (at src_w × src_h) → `self.output` (at dst_w × dst_h).
        /// Ends any active compute/render encoder before encoding MetalFX.
        pub fn upscale(
            &self,
            gpu: &mut GpuEncoder,
            source: &manifold_gpu::GpuTexture,
        ) {
            let cmd_buf = gpu.native_enc.raw_cmd_buf();
            self.scaler.encode(cmd_buf, source, &self.output.texture);
        }

        /// Resize both the scaler and the output texture when dimensions change.
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
            self.output.resize(device, dst_w, dst_h);
            true
        }
    }
}

#[cfg(target_os = "macos")]
pub use imp::MetalFxFullFrameUpscaler;
