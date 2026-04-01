// Compute-based blit helper for single-source effects.
//
// Replaces FragmentBlitHelper — compute dispatches avoid TBDR tile memory
// load/store overhead that render passes incur on every begin/end cycle.
// The compute encoder stays alive across dispatches, eliminating per-pass
// encoder creation cost.
//
// Compute shaders must define:
//   @group(0) @binding(0) var<uniform> uniforms: YourStruct;
//   @group(0) @binding(1) var source_tex: texture_2d<f32>;
//   @group(0) @binding(2) var tex_sampler: sampler;
//   @group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;
//
//   @compute @workgroup_size(16, 16)
//   fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) { ... }

use crate::gpu_encoder::GpuEncoder;

pub struct ComputeBlitHelper {
    pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,
}

impl ComputeBlitHelper {
    /// Create a new compute-based single-source effect pipeline.
    /// Shader must contain cs_main with @workgroup_size(16, 16).
    pub fn new(
        device: &manifold_gpu::GpuDevice,
        shader_source: &str,
        label: &str,
    ) -> Self {
        let pipeline = device.create_compute_pipeline(shader_source, "cs_main", label);
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());
        Self { pipeline, sampler }
    }

    /// Execute a compute dispatch with an externally-supplied specialized pipeline.
    /// Used for function-constant-specialized variants where the effect holds
    /// multiple pre-compiled pipelines and selects one per dispatch.
    pub fn dispatch_with(
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
        gpu.native_enc.dispatch_compute(
            pipeline,
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

    /// Execute a compute dispatch: reads source texture, writes to output storage texture.
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
        self.dispatch_with(
            &self.pipeline, gpu, source, target, uniform_bytes, label, width, height,
        );
    }
}
