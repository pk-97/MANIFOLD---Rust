/// GPU cache texture for a single UI panel region.
///
/// Each panel renders to its own offscreen texture. On clean frames,
/// the cached texture is composited directly without re-rendering.
#[derive(Default)]
pub struct PanelCacheTexture {
    texture: Option<wgpu::Texture>,
    view: Option<wgpu::TextureView>,
    bind_group: Option<wgpu::BindGroup>,
    width: u32,
    height: u32,
    valid: bool,
}

impl PanelCacheTexture {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ensure the texture matches the required dimensions.
    /// Recreates if size changed. Returns true if texture was (re)created.
    pub fn ensure_size(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        bind_group_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        width: u32,
        height: u32,
    ) -> bool {
        if width == 0 || height == 0 {
            return false;
        }
        if self.width == width && self.height == height && self.texture.is_some() {
            return false;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Panel Cache"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Panel Cache BG"),
            layout: bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        self.texture = Some(texture);
        self.view = Some(view);
        self.bind_group = Some(bind_group);
        self.width = width;
        self.height = height;
        self.valid = false;
        true
    }

    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    pub fn is_valid(&self) -> bool {
        self.valid
    }

    pub fn mark_valid(&mut self) {
        self.valid = true;
    }

    pub fn view(&self) -> Option<&wgpu::TextureView> {
        self.view.as_ref()
    }

    pub fn bind_group(&self) -> Option<&wgpu::BindGroup> {
        self.bind_group.as_ref()
    }
}
