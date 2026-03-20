/// Per-window surface state.
pub struct SurfaceWrapper {
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
}

impl SurfaceWrapper {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
        scale_factor: f64,
        present_mode: wgpu::PresentMode,
    ) -> Self {
        let surface = instance.create_surface(target).expect("Failed to create surface");

        let caps = surface.get_capabilities(adapter);
        let format = caps.formats.iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(caps.formats[0]);

        // Use requested present mode if supported, otherwise fall back
        let actual_present_mode = if caps.present_modes.contains(&present_mode) {
            present_mode
        } else {
            caps.present_modes[0]
        };

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: actual_present_mode,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(device, &config);

        Self { surface, config, width, height, scale_factor }
    }

    /// Create an HDR surface using Rgba16Float.
    /// wgpu v28 Metal backend automatically sets `wantsExtendedDynamicRangeContent = YES`
    /// when the surface format is Rgba16Float, enabling EDR output.
    /// Unity: NativeMonitorWindowController.cs + MonitorWindowPlugin.mm HDR path.
    #[allow(clippy::too_many_arguments)]
    pub fn new_hdr(
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
        scale_factor: f64,
        present_mode: wgpu::PresentMode,
    ) -> Self {
        let surface = instance.create_surface(target).expect("Failed to create surface");

        let caps = surface.get_capabilities(adapter);

        // Prefer Rgba16Float for HDR/EDR output.
        // Fallback: first available format (will be SDR but never fails).
        let format = if caps.formats.contains(&wgpu::TextureFormat::Rgba16Float) {
            wgpu::TextureFormat::Rgba16Float
        } else {
            log::warn!(
                "[OutputWindow] Rgba16Float not supported — HDR unavailable. Available: {:?}",
                caps.formats
            );
            caps.formats.iter()
                .find(|f| f.is_srgb())
                .copied()
                .unwrap_or(caps.formats[0])
        };

        let actual_present_mode = if caps.present_modes.contains(&present_mode) {
            present_mode
        } else {
            caps.present_modes[0]
        };

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: actual_present_mode,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(device, &config);

        if format == wgpu::TextureFormat::Rgba16Float {
            log::info!("[OutputWindow] HDR surface created (Rgba16Float + EDR)");
        }

        Self { surface, config, width, height, scale_factor }
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32, scale_factor: f64) {
        self.width = width.max(1);
        self.height = height.max(1);
        self.scale_factor = scale_factor;
        self.config.width = self.width;
        self.config.height = self.height;
        self.surface.configure(device, &self.config);
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn get_current_texture(&self) -> Result<wgpu::SurfaceTexture, wgpu::SurfaceError> {
        self.surface.get_current_texture()
    }
}
