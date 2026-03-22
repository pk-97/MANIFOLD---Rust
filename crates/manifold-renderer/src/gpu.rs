use std::sync::Arc;

/// Central GPU resource holder. Created once, shared across all windows and render targets.
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
}

impl GpuContext {
    /// Create a new GPU context. Async — use pollster::block_on at the call site.
    /// Pass a compatible surface to ensure the adapter can present to it.
    pub async fn new(compatible_surface: Option<&wgpu::Surface<'_>>) -> Self {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface,
                force_fallback_adapter: false,
            })
            .await
            .expect("Failed to find a suitable GPU adapter");

        log::info!("GPU adapter: {:?}", adapter.get_info().name);

        let (device, queue) = Self::create_device_from_adapter(&adapter, "MANIFOLD Device").await;

        Self { instance, adapter, device: Arc::new(device), queue: Arc::new(queue) }
    }

    /// Request an additional device from the same adapter.
    /// Used to create a separate device+queue for the content thread so
    /// heavy GPU compute cannot block UI rendering.
    pub async fn create_secondary_device(&self, label: &str) -> GpuDevice {
        let (device, queue) = Self::create_device_from_adapter(&self.adapter, label).await;
        log::info!("Created secondary GPU device: {}", label);
        GpuDevice {
            device: Arc::new(device),
            queue: Arc::new(queue),
        }
    }

    async fn create_device_from_adapter(
        adapter: &wgpu::Adapter,
        label: &str,
    ) -> (wgpu::Device, wgpu::Queue) {
        // Request TIMESTAMP_QUERY if the adapter supports it (for GPU profiling).
        let mut features = wgpu::Features::empty();
        if adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            features |= wgpu::Features::TIMESTAMP_QUERY;
        }

        adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some(label),
                required_features: features,
                required_limits: adapter.limits(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
                ..Default::default()
            })
            .await
            .expect("Failed to create GPU device")
    }
}

/// Standalone device+queue pair. Used by the content thread to isolate
/// heavy GPU compute from the UI thread's rendering.
pub struct GpuDevice {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
}
