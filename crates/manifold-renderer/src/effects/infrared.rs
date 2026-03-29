use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use super::fragment_blit_helper::FragmentBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InfraredUniforms {
    amount: f32,
    palette: f32,
    contrast: f32,
    _pad0: f32,
}

/// Infrared / thermal vision effect.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
/// Unity ref: InfraredFX.cs / InfraredEffect.shader
pub struct InfraredFX {
    helper: FragmentBlitHelper,
}

impl InfraredFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: FragmentBlitHelper::new(
                device,
                include_str!("shaders/fx_infrared.wgsl"),
                "Infrared",
            ),
        }
    }
}

impl PostProcessEffect for InfraredFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::INFRARED
    }

    fn should_skip(&self, fx: &EffectInstance) -> bool {
        fx.param_values.first().copied().unwrap_or(0.0) <= 0.0
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
        let uniforms = InfraredUniforms {
            amount:      p.first().copied().unwrap_or(0.0),
            palette:     p.get(1).copied().unwrap_or(0.0),
            contrast:    p.get(2).copied().unwrap_or(1.0),
            _pad0: 0.0,
        };

        self.helper.dispatch(
            gpu,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Infrared Pass",
            ctx.width, ctx.height,
        );
    }
}
