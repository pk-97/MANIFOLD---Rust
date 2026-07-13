/// Central GPU resource holder for the UI thread and content thread.
/// Thin wrapper around GpuDevice. `device` is an `Arc` (not an owned
/// `GpuDevice`) so callers that need to hand out a shared owning handle
/// (e.g. building a `PresetRuntime`/`MetalBackend` for a one-shot render)
/// can clone it instead of ever caching a raw pointer (BUG-054).
pub struct GpuContext {
    pub device: std::sync::Arc<manifold_gpu::GpuDevice>,
}

impl GpuContext {
    pub fn new() -> Self {
        Self {
            device: std::sync::Arc::new(manifold_gpu::GpuDevice::new()),
        }
    }
}

impl Default for GpuContext {
    fn default() -> Self {
        Self::new()
    }
}
