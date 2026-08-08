//! ML denoiser wrapper — MetalFX Temporal Denoised Scaler integration
//! (RAYTRACING_DESIGN.md section 17 DN-F/DN-G).
//!
//! Thin wrapper around `manifold_gpu::denoiser::GpuDenoiser` that hides
//! Apple types from callers above manifold-gpu (I-DN3). Follows the same
//! pattern as `metalfx_temporal_upscaler.rs`.
//!
//! The denoiser performs fused temporal denoising and optional upscaling
//! on the composited RT scene color (DN2). Input resolution (render res)
//! and output resolution (native res) are set at construction; 1:1 is
//! pure denoising with no upscale.

#[cfg(target_os = "macos")]
mod imp {
    use manifold_gpu::denoiser::GpuDenoiser;
    use crate::gpu_encoder::GpuEncoder;

    /// macOS-native MetalFX Temporal Denoised Scaler, wrapped to avoid
    /// leaking Apple types into render-scene code.
    pub struct Denoiser {
        inner: manifold_gpu::denoiser::MetalFxDenoisedScaler,
        pub input_width: u32,
        pub input_height: u32,
        pub output_width: u32,
        pub output_height: u32,
    }

    impl Denoiser {
        /// Create a denoiser for the given (input, output) dimensions.
        /// Returns `None` if MetalFX Temporal Denoised Scaler is not
        /// available on this device (pre-Tahoe — DN1 fallback).
        pub fn new(
            device: &manifold_gpu::GpuDevice,
            input_width: u32,
            input_height: u32,
            output_width: u32,
            output_height: u32,
            depth_reversed: bool,
        ) -> Option<Self> {
            let inner = manifold_gpu::denoiser::MetalFxDenoisedScaler::new(
                device.raw_device(),
                input_width,
                input_height,
                output_width,
                output_height,
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                depth_reversed,
            )?;
            Some(Self {
                inner,
                input_width,
                input_height,
                output_width,
                output_height,
            })
        }

        /// Set the per-frame jitter offset (in pixels). Call before
        /// [`encode`](Self::encode) each frame; set to (0, 0) when not
        /// jittering (1:1 denoise).
        ///
        /// D-64's lesson: the denoiser's internal jitterOffset compensates
        /// the CURRENT frame's jitter; motion vectors must arrive jitter-free
        /// (the WGSL fragment subtracts the delta via `velocity_jitter`).
        pub fn set_jitter(&self, x: f32, y: f32) {
            self.inner.set_jitter(x, y);
        }

        /// Encode the denoise (and optional upscale) into the current
        /// command buffer. Input textures must match the dimensions set at
        /// construction; output texture must be at `output_width` ×
        /// `output_height`.
        ///
        /// `reset` discards temporal history for this frame. Caller drives
        /// it from the shared reset signals (TemporalResetDetector +
        /// lighting-key + gesture flags — DN3).
        ///
        /// `reactive_mask` is the per-pixel reactivity hint; pass `None`
        /// when not available (the denoiser falls back to uniform reactivity).
        #[allow(clippy::too_many_arguments)]
        pub fn encode(
            &self,
            gpu: &mut GpuEncoder,
            color: &manifold_gpu::GpuTexture,
            depth: &manifold_gpu::GpuTexture,
            motion: &manifold_gpu::GpuTexture,
            normal: &manifold_gpu::GpuTexture,
            roughness: &manifold_gpu::GpuTexture,
            diffuse_albedo: &manifold_gpu::GpuTexture,
            specular_albedo: &manifold_gpu::GpuTexture,
            specular_hit_distance: &manifold_gpu::GpuTexture,
            output: &manifold_gpu::GpuTexture,
            reset: bool,
            reactive_mask: Option<&manifold_gpu::GpuTexture>,
        ) {
            let cmd_buf = gpu.native_enc.raw_cmd_buf();
            self.inner.encode(
                cmd_buf,
                color,
                depth,
                false, // depth_reversed: standard (0=near, 1=far)
                motion,
                normal,
                roughness,
                diffuse_albedo,
                specular_albedo,
                specular_hit_distance,
                None, // exposure — the denoiser normalises internally
                output,
                reset,
                reactive_mask,
            );
        }

        /// Whether this denoiser matches the given (input, output)
        /// dimensions for reuse.
        pub fn matches(
            &self,
            in_w: u32,
            in_h: u32,
            out_w: u32,
            out_h: u32,
        ) -> bool {
            self.input_width == in_w
                && self.input_height == in_h
                && self.output_width == out_w
                && self.output_height == out_h
        }
    }
}

#[cfg(target_os = "macos")]
pub use imp::Denoiser;
