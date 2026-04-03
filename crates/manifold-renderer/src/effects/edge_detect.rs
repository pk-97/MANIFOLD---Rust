use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdgeDetectUniforms {
    amount: f32,
    threshold: f32,
    texel_size_x: f32,
    texel_size_y: f32,
}

/// Edge detection effect — Sobel 3x3 edge detection.
/// Pure edge detect without glow. Use Bloom or Halation after for glow.
pub struct EdgeDetectFX {
    helper: ComputeBlitHelper,
}

impl EdgeDetectFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_edge_detect.wgsl"),
                "EdgeDetect",
            ),
        }
    }
}

impl PostProcessEffect for EdgeDetectFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::EDGE_DETECT
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
            amount: p.first().copied().unwrap_or(0.0),
            threshold: p.get(1).copied().unwrap_or(0.1),
            texel_size_x: 1.0 / ctx.output_width as f32,
            texel_size_y: 1.0 / ctx.output_height as f32,
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "EdgeDetect Pass",
            ctx.width,
            ctx.height,
        );
    }
}
