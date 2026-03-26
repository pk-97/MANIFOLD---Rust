use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat, GpuTextureDimension,
                   GpuTextureDesc, GpuTextureUsage};

/// Offscreen render texture for compositing.
pub struct RenderTarget {
    pub texture: GpuTexture,
    pub width: u32,
    pub height: u32,
    pub format: GpuTextureFormat,
    label: String,
}

impl RenderTarget {
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
        });
        Self { texture, width, height, format, label: label.to_string() }
    }

    pub fn resize(&mut self, device: &GpuDevice, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }
        *self = Self::new(device, width, height, self.format, &self.label);
    }
}
