//! Linear EDR → ST.2084 PQ encoder pipeline for HDR export.
//!
//! Takes the final compositor output (post-tonemap, post-effects in EDR
//! display-linear space) and encodes to PQ for HDR10 HEVC delivery.
//! Uses a compute dispatch via manifold-gpu for zero TBDR overhead.

use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;

/// Uniform buffer layout for the PQ encoder shader. 16-byte aligned.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PqUniforms {
    paper_white: f32,
    max_nits: f32,
    _pad0: f32,
    _pad1: f32,
}

/// GPU pipeline for EDR → PQ transfer function encoding.
pub struct PqEncoder {
    pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,
    /// PQ-encoded output buffer.
    pub output: RenderTarget,
}

impl PqEncoder {
    pub fn new(
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
    ) -> Self {
        let format = manifold_gpu::GpuTextureFormat::Rgba16Float;
        let pipeline = device.create_compute_pipeline(
            include_str!("effects/shaders/linear_to_pq_compute.wgsl"),
            "cs_main",
            "PQ Encoder",
        );
        let sampler =
            device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());
        let output = RenderTarget::new(device, width, height, format, "PQ Export Output");

        Self { pipeline, sampler, output }
    }

    /// Encode linear EDR → PQ. Source is the tonemap output (EDR display-linear).
    pub fn encode(
        &self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        paper_white_nits: f32,
        max_display_nits: f32,
    ) {
        let uniforms = PqUniforms {
            paper_white: paper_white_nits,
            max_nits: max_display_nits,
            _pad0: 0.0,
            _pad1: 0.0,
        };

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
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: &self.output.texture,
                },
            ],
            [
                self.output.width.div_ceil(16),
                self.output.height.div_ceil(16),
                1,
            ],
            "PQ Encode",
        );
    }

    /// Resize the output buffer.
    pub fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        self.output.resize(device, width, height);
    }
}
