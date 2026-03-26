// Compute-based blit helper for single-texture effects.
//
// Dispatches compute shaders via manifold_gpu::GpuEncoder directly.
// Uses inline bytes (Metal set_bytes) for uniforms — zero buffer allocation.
//
// Compute shaders must define:
//   @group(0) @binding(0) var<uniform> uniforms: YourStruct;
//   @group(0) @binding(1) var source_tex: texture_2d<f32>;
//   @group(0) @binding(2) var tex_sampler: sampler;
//   @group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

use crate::gpu_encoder::GpuEncoder;

pub struct ComputeBlitHelper {
    pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,
}

impl ComputeBlitHelper {
    /// Create a new compute-based effect pipeline.
    pub fn new(
        device: &manifold_gpu::GpuDevice,
        shader_source: &str,
        label: &str,
    ) -> Self {
        let pipeline = device.create_compute_pipeline(shader_source, "cs_main", label);
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());
        Self { pipeline, sampler }
    }

    /// Execute a compute dispatch: reads source texture, writes to target storage texture.
    /// Dispatches ceil(width/16) x ceil(height/16) workgroups of 16x16 threads.
    pub fn dispatch(
        &self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        uniform_bytes: &[u8],
        label: &str,
        width: u32,
        height: u32,
    ) {
        gpu.native_enc.dispatch_compute(
            &self.pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: uniform_bytes,
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
                    texture: target,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            label,
        );
    }
}
