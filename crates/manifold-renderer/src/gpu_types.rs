//! GPU resource handle types for the hal encoding migration.
//!
//! Phase 1: thin wrappers around wgpu types with Deref for compatibility.
//! Phase 2+: will cache hal pointers for zero-overhead hot-path access
//! when the `hal-encoding` feature is enabled.

use std::ops::Deref;

/// GPU texture wrapper.
pub struct GpuTexture {
    pub wgpu: wgpu::Texture,
}

impl GpuTexture {
    pub fn new(texture: wgpu::Texture) -> Self {
        Self { wgpu: texture }
    }

    pub fn create_view(&self, desc: &wgpu::TextureViewDescriptor) -> GpuTextureView {
        GpuTextureView::new(self.wgpu.create_view(desc))
    }

    pub fn create_default_view(&self) -> GpuTextureView {
        GpuTextureView::new(self.wgpu.create_view(&wgpu::TextureViewDescriptor::default()))
    }
}

impl Deref for GpuTexture {
    type Target = wgpu::Texture;
    fn deref(&self) -> &wgpu::Texture {
        &self.wgpu
    }
}

/// GPU texture view wrapper.
pub struct GpuTextureView {
    pub wgpu: wgpu::TextureView,
}

impl GpuTextureView {
    pub fn new(view: wgpu::TextureView) -> Self {
        Self { wgpu: view }
    }
}

impl Deref for GpuTextureView {
    type Target = wgpu::TextureView;
    fn deref(&self) -> &wgpu::TextureView {
        &self.wgpu
    }
}

impl Clone for GpuTextureView {
    fn clone(&self) -> Self {
        Self { wgpu: self.wgpu.clone() }
    }
}
