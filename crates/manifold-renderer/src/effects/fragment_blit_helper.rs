// Fragment-based blit helper for single-pass effects.
//
// Uses render passes (draw_fullscreen) via manifold_gpu::GpuEncoder.
// On Apple Silicon's TBDR architecture, fragment shaders benefit from
// tile memory, texture cache locality, and implicit LOD — outperforming
// compute for single-pass full-screen effects.
//
// Fragment shaders must define:
//   @group(0) @binding(0) var<uniform> uniforms: YourStruct;
//   @group(0) @binding(1) var source_tex: texture_2d<f32>;
//   @group(0) @binding(2) var tex_sampler: sampler;
//   // NO output storage texture — render target IS the output.
//
//   @vertex fn vs_main(...) -> VertexOutput { ... }
//   @fragment fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> { ... }

use crate::gpu_encoder::GpuEncoder;

pub struct FragmentBlitHelper {
    pipeline: manifold_gpu::GpuRenderPipeline,
    sampler: manifold_gpu::GpuSampler,
}

impl FragmentBlitHelper {
    /// Create a new fragment-based effect pipeline.
    /// Shader must contain vs_main (fullscreen triangle) and fs_main (effect).
    /// Target format: Rgba16Float (matches compute output format).
    pub fn new(
        device: &manifold_gpu::GpuDevice,
        shader_source: &str,
        label: &str,
    ) -> Self {
        let pipeline = device.create_render_pipeline(
            shader_source,
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            None,
            label,
        );
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());
        Self { pipeline, sampler }
    }

    /// Execute a fullscreen fragment pass with an externally-supplied specialized pipeline.
    /// Used for function-constant-specialized variants where the effect holds
    /// multiple pre-compiled pipelines and selects one per dispatch.
    pub fn dispatch_with(
        &self,
        pipeline: &manifold_gpu::GpuRenderPipeline,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        uniform_bytes: &[u8],
        label: &str,
        _width: u32,
        _height: u32,
    ) {
        gpu.native_enc.draw_fullscreen(
            pipeline,
            target,
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
            ],
            false,
            true,
            label,
        );
    }

    /// Execute a fullscreen fragment pass: reads source texture, writes to render target.
    /// Width/height are unused — render target size is implicit from the target texture.
    pub fn dispatch(
        &self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        uniform_bytes: &[u8],
        label: &str,
        _width: u32,
        _height: u32,
    ) {
        gpu.native_enc.draw_fullscreen(
            &self.pipeline,
            target,
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
            ],
            false,
            true,
            label,
        );
    }
}
