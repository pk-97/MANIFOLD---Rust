//! Unified GPU encoding context for the content thread hot path.
//!
//! Phase 1 (default): wraps wgpu types directly with public field access.
//! Phase 2+ (hal-encoding feature on macOS): hal command encoder backend that
//! bypasses wgpu's validation and double-encoding overhead.

/// GPU encoding context passed through the entire content thread render loop.
/// Replaces the `(device, queue, encoder)` triplet in all hot-path signatures.
///
/// When `hal-encoding` is OFF (default): thin wrapper around wgpu types.
/// When `hal-encoding` is ON (macOS only): owns a hal CommandEncoder for
/// zero-overhead GPU command recording.
pub struct GpuEncoder<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,

    // wgpu encoding path (default, or non-macOS with feature enabled)
    #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
    pub encoder: &'a mut wgpu::CommandEncoder,

    // hal encoding path (macOS with hal-encoding feature)
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub encoder: &'a mut wgpu::CommandEncoder,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_encoder: Option<crate::hal_context::MetalCommandEncoder>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub hal_ctx: Option<&'a crate::hal_context::HalContext>,
}

impl<'a> GpuEncoder<'a> {
    /// Create a GpuEncoder with the wgpu backend (default path).
    pub fn new(
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        encoder: &'a mut wgpu::CommandEncoder,
    ) -> Self {
        Self {
            device,
            queue,
            encoder,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_encoder: None,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_ctx: None,
        }
    }

    /// Create a GpuEncoder with the hal backend for zero-overhead encoding.
    /// The wgpu encoder is still required as an escape hatch for non-migrated
    /// code (generators, render passes that haven't been ported to hal yet).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub fn new_hal(
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        encoder: &'a mut wgpu::CommandEncoder,
        hal_ctx: &'a crate::hal_context::HalContext,
    ) -> Self {
        use wgpu::hal::CommandEncoder as HalCommandEncoder;

        let mut hal_enc = hal_ctx.create_command_encoder();
        unsafe {
            hal_enc
                .begin_encoding(Some("Content Frame"))
                .expect("Failed to begin hal encoding");
        }

        Self {
            device,
            queue,
            encoder,
            hal_encoder: Some(hal_enc),
            hal_ctx: Some(hal_ctx),
        }
    }

    /// Returns true if this encoder has a hal backend active.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub fn has_hal(&self) -> bool {
        self.hal_encoder.is_some()
    }

    // --- hal compute pass methods ---
    // Safety for all hal methods: caller must ensure the hal encoder is in the
    // correct state (pass open/closed) and that resources passed are valid for
    // the duration of the GPU command buffer.

    /// Begin a hal compute pass.
    ///
    /// # Safety
    /// No hal compute pass must be currently open on this encoder.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub unsafe fn hal_begin_compute_pass(&mut self, label: &str) {
        use wgpu::hal::CommandEncoder as HalCommandEncoder;
        if let Some(enc) = self.hal_encoder.as_mut() {
            unsafe {
                enc.begin_compute_pass(&wgpu::hal::ComputePassDescriptor {
                    label: Some(label),
                    timestamp_writes: None,
                });
            }
        }
    }

    /// End the current hal compute pass.
    ///
    /// # Safety
    /// A hal compute pass must be currently open.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub unsafe fn hal_end_compute_pass(&mut self) {
        use wgpu::hal::CommandEncoder as HalCommandEncoder;
        if let Some(enc) = self.hal_encoder.as_mut() {
            unsafe { enc.end_compute_pass(); }
        }
    }

    /// Set the compute pipeline for the current hal compute pass.
    ///
    /// # Safety
    /// A hal compute pass must be open. Pipeline must be valid.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub unsafe fn hal_set_compute_pipeline(
        &mut self,
        pipeline: &<wgpu::hal::api::Metal as wgpu::hal::Api>::ComputePipeline,
    ) {
        use wgpu::hal::CommandEncoder as HalCommandEncoder;
        if let Some(enc) = self.hal_encoder.as_mut() {
            unsafe { enc.set_compute_pipeline(pipeline); }
        }
    }

    /// Set a bind group on the hal encoder.
    ///
    /// # Safety
    /// A hal compute pass must be open. Layout and group must be compatible.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub unsafe fn hal_set_bind_group(
        &mut self,
        index: u32,
        layout: &<wgpu::hal::api::Metal as wgpu::hal::Api>::PipelineLayout,
        group: &<wgpu::hal::api::Metal as wgpu::hal::Api>::BindGroup,
        dynamic_offsets: &[wgpu::DynamicOffset],
    ) {
        use wgpu::hal::CommandEncoder as HalCommandEncoder;
        if let Some(enc) = self.hal_encoder.as_mut() {
            unsafe { enc.set_bind_group(layout, index, group, dynamic_offsets); }
        }
    }

    /// Dispatch compute workgroups via hal.
    ///
    /// # Safety
    /// A hal compute pass must be open with pipeline and bind groups set.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub unsafe fn hal_dispatch(&mut self, x: u32, y: u32, z: u32) {
        use wgpu::hal::CommandEncoder as HalCommandEncoder;
        if let Some(enc) = self.hal_encoder.as_mut() {
            unsafe { enc.dispatch([x, y, z]); }
        }
    }

    /// Finish hal encoding and return the command buffer for submission.
    /// After this call, the hal encoder is consumed and only the wgpu
    /// escape hatch remains functional.
    ///
    /// # Safety
    /// All hal passes must be closed before calling this.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub unsafe fn finish_hal(
        &mut self,
    ) -> Option<crate::hal_context::MetalCommandBuffer> {
        use wgpu::hal::CommandEncoder as HalCommandEncoder;
        self.hal_encoder.take().map(|mut enc| unsafe {
            enc.end_encoding().expect("Failed to end hal encoding")
        })
    }
}
