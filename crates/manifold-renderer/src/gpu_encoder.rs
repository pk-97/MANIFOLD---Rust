//! Unified GPU encoding context for the content thread hot path.
//!
//! Wraps wgpu types with public field access. When `hal-encoding` is enabled,
//! also carries a reference to `HalContext` for creating hal bind groups,
//! samplers, and other resources. Actual hal encoding happens via
//! `encoder.as_hal_mut()` inside effect/generator dispatch methods — NOT
//! via a separate hal command encoder.

/// GPU encoding context passed through the entire content thread render loop.
/// Replaces the `(device, queue, encoder)` triplet in all hot-path signatures.
///
/// hal dispatch pattern: effects call `gpu.encoder.as_hal_mut()` to encode
/// directly into the wgpu command buffer via hal. This gives zero-overhead
/// encoding while maintaining correct interleaving with wgpu-encoded work
/// (generators, blends, copies) in a single command buffer.
pub struct GpuEncoder<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// HalContext for hal resource creation (bind groups, samplers).
    /// None when hal-encoding feature is off or on non-macOS.
    pub hal_ctx: Option<&'a crate::hal_context::HalContext>,
}

impl<'a> GpuEncoder<'a> {
    pub fn new(
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        encoder: &'a mut wgpu::CommandEncoder,
        hal_ctx: Option<&'a crate::hal_context::HalContext>,
    ) -> Self {
        Self { device, queue, encoder, hal_ctx }
    }
}
