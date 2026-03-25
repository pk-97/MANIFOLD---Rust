//! HAL compute dispatch helpers for generators and effects.
//!
//! Provides utilities for extracting hal resource pointers from wgpu objects
//! and dispatching compute work through the hal command encoder.

#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
mod inner {
    use wgpu::hal::{self, CommandEncoder as HalCmdEnc, Device as HalDevice};

    use crate::hal_context::HalContext;
    use crate::hal_pipeline::HalComputePipeline;

    pub type MetalApi = hal::api::Metal;
    pub type MetalCommandEncoder = <MetalApi as hal::Api>::CommandEncoder;
    pub type MetalTextureView = <MetalApi as hal::Api>::TextureView;
    pub type MetalBuffer = <MetalApi as hal::Api>::Buffer;
    pub type MetalSampler = <MetalApi as hal::Api>::Sampler;
    pub type MetalBindGroup = <MetalApi as hal::Api>::BindGroup;

    /// Extract a hal texture view pointer from a wgpu TextureView.
    ///
    /// The returned pointer is valid as long as the TextureView is alive
    /// (i.e., for the duration of the current frame).
    ///
    /// # Safety
    /// Caller must ensure the TextureView outlives all uses of the pointer.
    #[inline]
    pub unsafe fn extract_hal_view(
        view: &wgpu::TextureView,
    ) -> *const MetalTextureView {
        let guard = unsafe {
            view.as_hal::<MetalApi>()
                .expect("TextureView not Metal")
        };
        &*guard as *const MetalTextureView
    }

    /// Extract a hal buffer pointer from a wgpu Buffer.
    ///
    /// # Safety
    /// Caller must ensure the Buffer outlives all uses of the pointer.
    #[inline]
    pub unsafe fn extract_hal_buffer(
        buffer: &wgpu::Buffer,
    ) -> *const MetalBuffer {
        let guard = unsafe {
            buffer.as_hal::<MetalApi>()
                .expect("Buffer not Metal")
        };
        &*guard as *const MetalBuffer
    }

    /// Encode a compute dispatch via the hal command encoder.
    ///
    /// Takes a pre-created hal bind group, dispatches the compute work,
    /// then destroys the bind group. Encapsulates the standard 7-step
    /// hal dispatch pattern.
    ///
    /// # Safety
    /// - `hal_enc` must be a valid, active hal command encoder
    /// - `bg` must be a valid hal bind group compatible with `pipeline`
    pub unsafe fn dispatch_hal_compute(
        hal_enc: &mut MetalCommandEncoder,
        hal_ctx: &HalContext,
        pipeline: &HalComputePipeline,
        bg: MetalBindGroup,
        dynamic_offsets: &[u32],
        workgroups: [u32; 3],
        label: &str,
    ) {
        unsafe {
            hal_enc.begin_compute_pass(&hal::ComputePassDescriptor {
                label: Some(label),
                timestamp_writes: None,
            });
            hal_enc.set_compute_pipeline(&pipeline.pipeline);
            hal_enc.set_bind_group(
                &pipeline.pipeline_layout,
                0,
                &bg,
                dynamic_offsets,
            );
            hal_enc.dispatch(workgroups);
            hal_enc.end_compute_pass();
            hal_ctx.device().destroy_bind_group(bg);
        }
    }
}

#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
pub use inner::*;
