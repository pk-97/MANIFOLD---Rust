//! wgpu backend for Windows/Linux — correctness over performance.
//!
//! Wraps wgpu types to provide the same GpuDevice/GpuEncoder/etc API
//! as the Metal backend. Uses standard wgpu command encoding and submission.
//!
//! This backend exists for cross-platform correctness. Performance
//! optimizations are Metal-first.

// TODO: Implement wgpu backend when Windows/Linux support is needed.
// For now, this is a placeholder that allows the crate to compile
// on non-macOS platforms.

use crate::types::*;

pub struct GpuDevice;
pub struct GpuEncoder;
pub struct GpuTexture {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub format: GpuTextureFormat,
}
pub struct GpuBuffer {
    pub size: u64,
}
pub struct GpuSampler;
pub struct GpuComputePipeline {
    pub label: String,
    pub workgroup_size: [u32; 3],
}
pub struct GpuRenderPipeline {
    pub label: String,
}
pub struct GpuEvent;

impl GpuDevice {
    pub fn new() -> Self { todo!("wgpu backend not yet implemented") }
}
