use crate::render_target::RenderTarget;
use manifold_gpu::GpuTextureFormat;

/// Ping-pong state management for simulation generators.
/// Owns two RenderTargets and alternates read/write roles each step.
pub struct StatefulState {
    state_a: RenderTarget,
    state_b: RenderTarget,
    use_a: bool,
    frame_count: u32,
}

impl StatefulState {
    pub fn new(
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        label: &str,
    ) -> Self {
        let state_a = RenderTarget::new(
            device, width, height, format, &format!("{} State A", label),
        );
        let state_b = RenderTarget::new(
            device, width, height, format, &format!("{} State B", label),
        );
        Self {
            state_a,
            state_b,
            use_a: true,
            frame_count: 0,
        }
    }

    /// The texture to sample from (previous frame's output).
    pub fn read_texture(&self) -> &manifold_gpu::GpuTexture {
        if self.use_a {
            &self.state_a.texture
        } else {
            &self.state_b.texture
        }
    }

    /// The texture to render into (current frame's output).
    pub fn write_texture(&self) -> &manifold_gpu::GpuTexture {
        if self.use_a {
            &self.state_b.texture
        } else {
            &self.state_a.texture
        }
    }

    /// Swap read/write roles after a simulation step.
    pub fn swap(&mut self) {
        self.use_a = !self.use_a;
        self.frame_count += 1;
    }

    /// Recreate both state textures at a new resolution.
    pub fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        self.state_a.resize(device, width, height);
        self.state_b.resize(device, width, height);
        self.frame_count = 0;
    }

    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }

    pub fn width(&self) -> u32 {
        self.state_a.width
    }

    pub fn height(&self) -> u32 {
        self.state_a.height
    }
}
