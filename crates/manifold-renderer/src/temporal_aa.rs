//! Temporal Anti-Aliasing pass for native resolution rendering.
//!
//! When render_scale == 1.0 (no upscaling), generators render with sub-pixel
//! jitter (Halton sequence) and this pass blends the current frame with an
//! exponentially-weighted history buffer. Over ~8 frames, each pixel accumulates
//! real sub-pixel samples — free supersampling without resolution cost.
//!
//! The neighbourhood clamp in the shader prevents ghosting on moving content
//! by restricting history to the local 3×3 min/max of the current frame.

use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;

/// Uniform layout for the TAA blend pass. 16-byte aligned.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TaaUniforms {
    blend_weight: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// Temporal AA pass: jitter accumulation with neighbourhood-clamped history blend.
pub struct TemporalAAPass {
    pipeline: manifold_gpu::GpuComputePipeline,
    /// History buffer — stores the blended result from previous frames.
    history: RenderTarget,
    /// Output buffer — TAA writes here, then we swap with history.
    output: RenderTarget,
    pub width: u32,
    pub height: u32,
    /// True on first frame or after resize — skip history blend.
    needs_reset: bool,
}

impl TemporalAAPass {
    pub fn new(device: &manifold_gpu::GpuDevice, width: u32, height: u32) -> Self {
        let fmt = manifold_gpu::GpuTextureFormat::Rgba16Float;
        let pipeline = device.create_compute_pipeline(
            include_str!("effects/shaders/temporal_aa_compute.wgsl"),
            "cs_main",
            "Temporal AA",
        );
        let history = RenderTarget::new(device, width, height, fmt, "TAA History");
        let output = RenderTarget::new(device, width, height, fmt, "TAA Output");
        Self { pipeline, history, output, width, height, needs_reset: true }
    }

    /// Apply temporal AA: blend `source` (current jittered frame) with history.
    /// Returns the blended output texture.
    pub fn apply<'a>(
        &'a mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
    ) -> &'a manifold_gpu::GpuTexture {
        if self.needs_reset {
            // First frame: copy source directly to history, skip blend.
            gpu.copy_texture_to_texture(source, &self.history.texture, self.width, self.height);
            self.needs_reset = false;
            return &self.history.texture;
        }

        // Balanced blend weight: good AA with minimal ghosting on motion.
        let uniforms = TaaUniforms {
            blend_weight: 0.15,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        // Blend: current + history → output
        gpu.native_enc.dispatch_compute(
            &self.pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: source,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: &self.history.texture,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: &self.output.texture,
                },
            ],
            [self.width.div_ceil(16), self.height.div_ceil(16), 1],
            "Temporal AA",
        );

        // Copy output → history for next frame's blend input.
        gpu.copy_texture_to_texture(&self.output.texture, &self.history.texture, self.width, self.height);

        &self.output.texture
    }

    /// Resize history and output buffers.
    pub fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.history.resize(device, width, height);
        self.output.resize(device, width, height);
        self.needs_reset = true;
    }

    /// Reset temporal history (e.g., after seek).
    pub fn reset(&mut self) {
        self.needs_reset = true;
    }
}
