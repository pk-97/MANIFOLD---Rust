//! Pre-tonemap luminance stabilizer — eliminates subtle brightness flickering
//! on noisy scenes caused by Jensen's inequality through the ACES tonemap curve.
//!
//! Each frame:
//! 1. CPU reads the compensation factor from the previous frame (1-frame delay).
//! 2. GPU dispatches a 256-thread luminance reduction on the pre-tonemap HDR buffer.
//! 3. The shader computes mean luminance, blends with an exponential moving average,
//!    and writes a new compensation factor for the next frame.
//!
//! The compensation is a single scalar multiplied into the tonemap exposure uniform.
//! No spatial blur, no temporal pixel blending, no ghosting.

use manifold_gpu::{
    GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuSampler, GpuSamplerDesc,
    GpuTexture,
};

/// GPU-accelerated luminance smoother with 1-frame-delayed readback.
pub struct LuminanceSmoother {
    pipeline: GpuComputePipeline,
    sampler: GpuSampler,
    /// Shared-memory buffer: `{ smoothed_lum: f32, compensation: f32 }`.
    /// GPU writes each frame; CPU reads at the start of the next frame.
    state_buf: GpuBuffer,
}

impl LuminanceSmoother {
    pub fn new(device: &GpuDevice) -> Self {
        let pipeline = device.create_compute_pipeline(
            include_str!("effects/shaders/luminance_reduce.wgsl"),
            "cs_main",
            "Luminance Reduce",
        );

        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        // 8 bytes: [smoothed_lum: f32, compensation: f32]
        // StorageModeShared for CPU readback without blit.
        let state_buf = device.create_buffer_shared(8);

        // Zero-initialize (smoothed_lum = 0 triggers first-frame seed in shader).
        unsafe { state_buf.write(0, &[0u8; 8]) };

        Self {
            pipeline,
            sampler,
            state_buf,
        }
    }

    /// Read the compensation factor computed by the previous frame's GPU dispatch.
    /// Returns 1.0 on the first frame or if the value is invalid.
    ///
    /// Safe to call because the content pipeline waits for the previous frame's
    /// GPU completion before starting a new frame (`wait_for_surface`).
    pub fn compensation(&self) -> f32 {
        let Some(ptr) = self.state_buf.mapped_ptr() else {
            return 1.0;
        };
        unsafe {
            // compensation is at offset 4 (second f32 in the struct).
            let comp = (ptr as *const f32).add(1).read();
            if comp > 0.0 && comp.is_finite() {
                comp
            } else {
                1.0
            }
        }
    }

    /// Dispatch the luminance reduction on the pre-tonemap HDR buffer.
    /// Must be called BEFORE the tonemap dispatch (same command buffer).
    /// The result is consumed next frame via `compensation()`.
    pub fn measure(
        &self,
        gpu: &mut crate::gpu_encoder::GpuEncoder,
        hdr_source: &GpuTexture,
    ) {
        gpu.native_enc.dispatch_compute(
            &self.pipeline,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: hdr_source,
                },
                GpuBinding::Sampler {
                    binding: 1,
                    sampler: &self.sampler,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &self.state_buf,
                    offset: 0,
                },
            ],
            [1, 1, 1], // single workgroup of 256 threads
            "Luminance Reduce",
        );
    }
}
