use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use super::fragment_blit_helper::FragmentBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdgeDetectUniforms {
    amount: f32,
    threshold: f32,
    mode: u32,
    texel_size_x: f32,
    texel_size_y: f32,
    _pad: [f32; 3],
}

/// Edge Detect WGSL source — shared across all specialized mode variants.
const EDGE_DETECT_WGSL: &str = include_str!("shaders/fx_edge_glow.wgsl");

/// Edge detection effect — Sobel, Laplacian, or Frei-Chen edge detection.
/// Pure edge detect without glow. Use Bloom or Halation after for glow.
/// Stateless single-pass fragment shader.
pub struct EdgeGlowFX {
    helper: FragmentBlitHelper,
    /// Specialized render pipelines per edge detection mode: Sobel=0, Laplacian=1, Frei-Chen=2.
    pipeline_sobel: manifold_gpu::GpuRenderPipeline,
    pipeline_laplacian: manifold_gpu::GpuRenderPipeline,
    pipeline_frei_chen: manifold_gpu::GpuRenderPipeline,
}

impl EdgeGlowFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let spec = |mode: &str, label: &str| {
            device.create_specialized_render_pipeline(
                EDGE_DETECT_WGSL,
                "vs_main",
                "fs_main",
                &[("uniforms.mode", mode)],
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                label,
            )
        };
        Self {
            helper: FragmentBlitHelper::new(device, EDGE_DETECT_WGSL, "EdgeDetect"),
            pipeline_sobel: spec("0u", "EdgeDetect Sobel"),
            pipeline_laplacian: spec("1u", "EdgeDetect Laplacian"),
            pipeline_frei_chen: spec("2u", "EdgeDetect Frei-Chen"),
        }
    }
}

impl PostProcessEffect for EdgeGlowFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::EDGE_GLOW
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        let p = &fx.param_values;
        let uniforms = EdgeDetectUniforms {
            amount:       p.first().copied().unwrap_or(0.0),
            threshold:    p.get(1).copied().unwrap_or(0.3),
            // p[2] was glow — ignored, kept for project file compatibility
            mode:         p.get(3).copied().unwrap_or(0.0).round() as u32,
            texel_size_x: 1.0 / ctx.width as f32,
            texel_size_y: 1.0 / ctx.height as f32,
            _pad: [0.0; 3],
        };

        let pipeline = match uniforms.mode {
            1 => &self.pipeline_laplacian,
            2 => &self.pipeline_frei_chen,
            _ => &self.pipeline_sobel,
        };
        self.helper.dispatch_with(
            pipeline, gpu,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "EdgeDetect Pass",
            ctx.width, ctx.height,
        );
    }
}
