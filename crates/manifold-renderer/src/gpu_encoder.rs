//! Unified GPU encoding context for the content thread hot path.
//!
//! Phase 1: wraps wgpu types directly with public field access.
//! Phase 2+: hal encoding on macOS when `hal-encoding` feature is enabled.

/// GPU encoding context passed through the entire content thread render loop.
/// Replaces the `(device, queue, encoder)` triplet in all hot-path signatures.
pub struct GpuEncoder<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
}

impl<'a> GpuEncoder<'a> {
    pub fn new(
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        encoder: &'a mut wgpu::CommandEncoder,
    ) -> Self {
        Self { device, queue, encoder }
    }
}
