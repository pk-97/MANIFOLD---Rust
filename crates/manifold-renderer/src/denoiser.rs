//! ML denoiser wrapper — MetalFX Temporal Denoised Scaler integration
//! (RAYTRACING_DESIGN.md section 17 DN-F/DN-G + 17.7 DN-K).
//!
//! Thin wrapper around MetalFX denoisers that hides Apple types from
//! callers above manifold-gpu (I-DN3). Follows the same pattern as
//! `metalfx_temporal_upscaler.rs`.
//!
//! The denoiser performs fused temporal denoising and optional upscaling
//! on the composited RT scene color (DN2). Input resolution (render res)
//! and output resolution (native res) is set at construction; 1:1 is
//! pure denoising with no upscale.
//!
//! **Metal 4 preference (DN-K):** when MTL4FXTemporalDenoisedScaler is
//! available, this wrapper uses it as the PREFERRED implementation with
//! GPU-side synchronization (no CPU stalls). Falls back to classic
//! MTLFX when unavailable.

#[cfg(target_os = "macos")]
mod imp {
    use crate::gpu_encoder::GpuEncoder;
    use manifold_gpu::denoiser::GpuDenoiser;
    use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};

    /// macOS-native MetalFX Temporal Denoised Scaler, wrapped to avoid
    /// leaking Apple types into render-scene code.
    ///
    /// Internally prefers `Metal4FxDenoisedScaler` (MTL4) and falls back
    /// to `MetalFxDenoisedScaler` (classic MTLFX) when unavailable.
    pub struct Denoiser {
        /// Classic MTLFX scaler (fallback).
        classic: Option<manifold_gpu::denoiser::MetalFxDenoisedScaler>,
        /// MTL4 scaler with GPU-side sync (preferred).
        m4: Option<manifold_gpu::metalfx_m4::Metal4FxDenoisedScaler>,
        pub input_width: u32,
        pub input_height: u32,
        pub output_width: u32,
        pub output_height: u32,
    }

    impl Denoiser {
        /// Create a denoiser for the given (input, output) dimensions.
        /// Returns `None` if MetalFX Temporal Denoised Scaler is not
        /// available on this device (pre-Tahoe — DN1 fallback).
        ///
        /// Prefers MTL4 when available (DN-K — preferred implementation),
        /// falls back to classic MTLFX.
        pub fn new(
            device: &GpuDevice,
            input_width: u32,
            input_height: u32,
            output_width: u32,
            output_height: u32,
            depth_reversed: bool,
        ) -> Option<Self> {
            let raw_device = device.raw_device();

            // MTL4 is preferred (DN-K). Share the per-device bridge so only
            // one MTL4 command queue + allocator exists per GpuDevice.
            let m4 = device.mtl4_bridge().and_then(|bridge| {
                manifold_gpu::metalfx_m4::Metal4FxDenoisedScaler::new(
                    raw_device,
                    bridge,
                    input_width,
                    input_height,
                    output_width,
                    output_height,
                    GpuTextureFormat::Rgba16Float,
                    depth_reversed,
                )
            });

            // Fallback to classic MTLFX if MTL4 unavailable.
            let classic = if m4.is_none() {
                manifold_gpu::denoiser::MetalFxDenoisedScaler::new(
                    raw_device,
                    input_width,
                    input_height,
                    output_width,
                    output_height,
                    GpuTextureFormat::Rgba16Float,
                    depth_reversed,
                )
            } else {
                None
            };

            // Both failed?
            if m4.is_none() && classic.is_none() {
                return None;
            }

            Some(Self {
                classic,
                m4,
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
            if let Some(ref m4) = self.m4 {
                m4.set_jitter(x, y);
            } else if let Some(ref classic) = self.classic {
                classic.set_jitter(x, y);
            }
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
        /// Returns `true` if the scaler wrote to `output`. On the MTL4 path,
        /// returns `false` when the per-device allocator ring is saturated
        /// and the frame was skipped; the caller must present fallback output
        /// (the `output` texture is untouched).
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
            output: &GpuTexture,
            reset: bool,
            reactive_mask: Option<&GpuTexture>,
        ) -> bool {
            let cmd_buf = gpu.native_enc.raw_cmd_buf();

            if let Some(ref m4) = self.m4 {
                // MTL4 path: encode handles GPU-side synchronization internally.
                // No CPU stalls, fully pipelined. May skip if the allocator ring
                // is saturated (too many frames in flight).
                m4.encode(
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
                )
            } else if let Some(ref classic) = self.classic {
                // Classic MTLFX path: direct encode into the same command buffer.
                classic.encode(
                    cmd_buf,
                    color,
                    depth,
                    false,
                    motion,
                    normal,
                    roughness,
                    diffuse_albedo,
                    specular_albedo,
                    specular_hit_distance,
                    None,
                    output,
                    reset,
                    reactive_mask,
                );
                true
            } else {
                false
            }
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
