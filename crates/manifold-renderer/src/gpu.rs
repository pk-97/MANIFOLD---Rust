/// Central GPU resource holder for the UI thread and content thread.
/// Thin wrapper around GpuDevice. The wgpu GPU context has been removed.
pub struct GpuContext {
    pub device: manifold_gpu::GpuDevice,
}

impl GpuContext {
    pub fn new() -> Self {
        Self { device: manifold_gpu::GpuDevice::new() }
    }
}

impl Default for GpuContext {
    fn default() -> Self {
        Self::new()
    }
}
