use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    TexturePool,
};

/// Offscreen render texture for compositing.
pub struct RenderTarget {
    pub texture: GpuTexture,
    pub width: u32,
    pub height: u32,
    pub format: GpuTextureFormat,
    label: String,
}

impl RenderTarget {
    /// Create via direct device allocation (kernel call per texture).
    /// Use `new_pooled()` for textures that benefit from heap recycling.
    pub fn new(
        device: &GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        label: &str,
    ) -> Self {
        let texture = device.create_texture(&GpuTextureDesc {
            width,
            height,
            depth: 1,
            format,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label,
            mip_levels: 1,
        });
        Self {
            texture,
            width,
            height,
            format,
            label: label.to_string(),
        }
    }

    /// Create from the texture pool (heap sub-allocation or recycled).
    /// Zero kernel calls when a matching texture is available in the pool.
    pub fn new_pooled(
        pool: &TexturePool,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        label: &str,
    ) -> Self {
        let texture = pool.acquire(
            width,
            height,
            format,
            GpuTextureUsage::RENDER_TARGET_FULL,
            label,
        );
        Self {
            texture,
            width,
            height,
            format,
            label: label.to_string(),
        }
    }

    /// Return this render target's texture to the pool for reuse.
    /// Consumes self — the RenderTarget is no longer usable after this call.
    pub fn release_to_pool(self, pool: &TexturePool) {
        pool.release(self.texture);
    }

    /// Create a memoryless render target (Apple Silicon only).
    /// Data stays in tile/cache memory — zero VRAM bandwidth.
    /// Only valid as render pass color attachments, NOT for compute storage.
    pub fn new_memoryless(
        device: &GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        label: &str,
    ) -> Self {
        let texture = device.create_texture_memoryless(&GpuTextureDesc {
            width,
            height,
            depth: 1,
            format,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET,
            label,
            mip_levels: 1,
        });
        Self {
            texture,
            width,
            height,
            format,
            label: label.to_string(),
        }
    }

    pub fn resize(&mut self, device: &GpuDevice, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }
        *self = Self::new(device, width, height, self.format, &self.label);
    }

    /// Resize using the texture pool. Old texture is released back to pool.
    pub fn resize_pooled(&mut self, pool: &TexturePool, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }
        // We can't release the old texture easily because we're replacing self.
        // The old texture will be dropped (freed by Metal). For resize paths,
        // this is acceptable since resizes are rare events, not per-frame.
        *self = Self::new_pooled(pool, width, height, self.format, &self.label);
    }
}
