//! GPU pipeline for wet/dry lerp blending in effect groups.
//!
//! Matches Unity's GroupWetDryLerp.shader: `lerp(dry, wet, wetDry)`.
//! Uses a compute dispatch via manifold-gpu for zero TBDR overhead.

use crate::gpu_encoder::GpuEncoder;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WetDryUniforms {
    wet_dry: f32,
    _pad: [f32; 3],
}

pub struct WetDryLerpPipeline {
    pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,
}

impl WetDryLerpPipeline {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let pipeline = device.create_compute_pipeline(
            include_str!("effects/shaders/wet_dry_lerp_compute.wgsl"),
            "cs_main",
            "WetDry Lerp Compute",
        );
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());
        Self { pipeline, sampler }
    }

    /// Apply wet/dry lerp: output = lerp(dry_snapshot, wet_result, wet_dry).
    /// Bindings: 0=uniforms, 1=dry, 2=wet, 3=sampler, 4=output
    pub fn apply(
        &self,
        gpu: &mut GpuEncoder,
        dry: &manifold_gpu::GpuTexture,
        wet: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        wet_dry: f32,
        width: u32,
        height: u32,
    ) {
        let uniforms = WetDryUniforms {
            wet_dry,
            _pad: [0.0; 3],
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
                    texture: dry,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: wet,
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
            "WetDry Lerp",
        );
    }
}
