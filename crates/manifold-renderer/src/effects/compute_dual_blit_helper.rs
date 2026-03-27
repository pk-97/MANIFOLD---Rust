// Compute-based dual-texture blit helper for effects that read two inputs.
//
// Same pattern as ComputeBlitHelper but with 5 bindings (two source textures).
//
// Compute shaders must define:
//   @group(0) @binding(0) var<uniform> uniforms: YourStruct;
//   @group(0) @binding(1) var source_a: texture_2d<f32>;
//   @group(0) @binding(2) var source_b: texture_2d<f32>;
//   @group(0) @binding(3) var tex_sampler: sampler;
//   @group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

use crate::gpu_encoder::GpuEncoder;

pub struct ComputeDualBlitHelper {
    pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,
}

impl ComputeDualBlitHelper {
    /// Create a new compute-based dual-texture effect pipeline.
    pub fn new(
        device: &manifold_gpu::GpuDevice,
        shader_source: &str,
        label: &str,
    ) -> Self {
        let pipeline = device.create_compute_pipeline(shader_source, "cs_main", label);
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());
        Self { pipeline, sampler }
    }

    /// Execute a compute dispatch with an externally-supplied specialized pipeline
    /// and one source texture (source_a only).
    pub fn dispatch_a_only_with(
        &self,
        pipeline: &manifold_gpu::GpuComputePipeline,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        uniform_bytes: &[u8],
        label: &str,
        width: u32,
        height: u32,
    ) {
        self.dispatch_with(
            pipeline, gpu, source, source, target, uniform_bytes, label, width, height,
        );
    }

    /// Execute a compute dispatch with an externally-supplied specialized pipeline
    /// and two source textures.
    pub fn dispatch_with(
        &self,
        pipeline: &manifold_gpu::GpuComputePipeline,
        gpu: &mut GpuEncoder,
        source_a: &manifold_gpu::GpuTexture,
        source_b: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        uniform_bytes: &[u8],
        label: &str,
        width: u32,
        height: u32,
    ) {
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: uniform_bytes,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: source_a,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: source_b,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 3,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 4,
                    texture: target,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            label,
        );
    }

    /// Execute a compute dispatch with one source texture (source_a only).
    /// Binds source_a to both @binding(1) and @binding(2) — the shader's
    /// source_b input is unused for prefilter/downsample modes.
    pub fn dispatch_a_only(
        &self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        uniform_bytes: &[u8],
        label: &str,
        width: u32,
        height: u32,
    ) {
        self.dispatch(gpu, source, source, target, uniform_bytes, label, width, height);
    }

    /// Execute a compute dispatch with two source textures.
    pub fn dispatch(
        &self,
        gpu: &mut GpuEncoder,
        source_a: &manifold_gpu::GpuTexture,
        source_b: &manifold_gpu::GpuTexture,
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
                    texture: source_a,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: source_b,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 3,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 4,
                    texture: target,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            label,
        );
    }
}
