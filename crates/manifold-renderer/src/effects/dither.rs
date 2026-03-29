use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use super::fragment_blit_helper::FragmentBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DitherUniforms {
    amount: f32,
    algorithm: u32,    // 0=Bayer,1=Halftone,2=Lines,3=CrossHatch,4=Noise,5=Diamond
    resolution_x: f32,
    resolution_y: f32,
}

/// Dither effect — 6 dithering algorithms with luminance-preserving quantization.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct DitherFX {
    helper: FragmentBlitHelper,
}

impl DitherFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: FragmentBlitHelper::new(
                device,
                include_str!("shaders/fx_dither.wgsl"),
                "Dither",
            ),
        }
    }
}

impl PostProcessEffect for DitherFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::DITHER
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
        let uniforms = DitherUniforms {
            amount: p.first().copied().unwrap_or(0.0),
            algorithm: (p.get(1).copied().unwrap_or(0.0).round() as u32).min(5),
            resolution_x: ctx.output_width as f32,
            resolution_y: ctx.output_height as f32,
        };

        self.helper.dispatch(
            gpu,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Dither Pass",
            ctx.width, ctx.height,
        );
    }
}
