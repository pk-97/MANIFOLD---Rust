//! ML denoiser wrapper — MetalFX Temporal Denoised Scaler integration
//! (RAYTRACING_DESIGN.md section 17 DN-F/DN-G + 17.7 DN-K).
//!
//! Pairs `manifold_gpu::GpuTemporalDenoiser` (which keeps the Apple types
//! inside manifold-gpu, I-DN3) with the render target it writes. Follows
//! the same pattern as `metalfx_temporal_upscaler.rs`.
//!
//! The denoiser performs fused temporal denoising and optional upscaling
//! on the composited RT scene color (DN2). Input resolution (render res)
//! and output resolution (native res) is set at construction; 1:1 is
//! pure denoising with no upscale. Off the Mac it is unavailable until
//! Vulkan's P7.

mod imp {
    use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
    use manifold_node_engine::gpu::render_target::RenderTarget;

    /// The ray-tracing denoiser (MetalFX Temporal Denoised Scaler on the
    /// Mac) and the render target it writes.
    pub struct Denoiser {
        inner: manifold_gpu::GpuTemporalDenoiser,
        pub input_width: u32,
        pub input_height: u32,
        pub output_width: u32,
        pub output_height: u32,
        pub depth_reversed: bool,
        pub output: RenderTarget,
    }

    impl Denoiser {
        /// Create a denoiser for the given (input, output) dimensions.
        /// Returns `None` if no denoiser is available on this device
        /// (pre-Tahoe — DN1 fallback).
        pub fn new(
            device: &GpuDevice,
            input_width: u32,
            input_height: u32,
            output_width: u32,
            output_height: u32,
            depth_reversed: bool,
        ) -> Option<Self> {
            let inner = manifold_gpu::GpuTemporalDenoiser::new(
                device,
                input_width,
                input_height,
                output_width,
                output_height,
                depth_reversed,
            )?;
            let output = RenderTarget::new(
                device,
                output_width,
                output_height,
                GpuTextureFormat::Rgba16Float,
                "MetalFX Denoiser Output",
            );
            Some(Self {
                inner,
                input_width,
                input_height,
                output_width,
                output_height,
                depth_reversed,
                output,
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
        ///
        /// Returns `true` if the scaler wrote to `output`. `false` when the
        /// frame was skipped under backpressure; the caller must present
        /// fallback output (the `output` texture is untouched).
        #[allow(clippy::too_many_arguments)]
        pub fn encode(
            &self,
            gpu: &mut GpuEncoder,
            color: &GpuTexture,
            depth: &GpuTexture,
            motion: &GpuTexture,
            normal: &GpuTexture,
            roughness: &GpuTexture,
            diffuse_albedo: &GpuTexture,
            specular_albedo: &GpuTexture,
            specular_hit_distance: &GpuTexture,
            reset: bool,
            reactive_mask: Option<&GpuTexture>,
        ) -> bool {
            self.inner.encode(
                gpu.native_enc,
                color,
                depth,
                motion,
                normal,
                roughness,
                diffuse_albedo,
                specular_albedo,
                specular_hit_distance,
                &self.output.texture,
                reset,
                reactive_mask,
            )
        }

        /// Whether this denoiser matches the given (input, output)
        /// dimensions for reuse.
        pub fn matches(&self, in_w: u32, in_h: u32, out_w: u32, out_h: u32) -> bool {
            self.input_width == in_w
                && self.input_height == in_h
                && self.output_width == out_w
                && self.output_height == out_h
        }
    }
}

pub use imp::Denoiser;
