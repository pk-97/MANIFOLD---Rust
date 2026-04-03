use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadMirrorUniforms {
    amount: f32,
    _pad: [f32; 3],
}

/// QuadMirror effect — mirrors UVs around center in both axes with crossfade.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct QuadMirrorFX {
    helper: ComputeBlitHelper,
}

impl QuadMirrorFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_quad_mirror.wgsl"),
                "QuadMirror",
            ),
        }
    }
}

impl PostProcessEffect for QuadMirrorFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::QUAD_MIRROR
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        // QuadMirrorFX.cs:13 — fx.GetParam(0), registry default 1.0
        let amount = fx.param_values.first().copied().unwrap_or(1.0);
        let uniforms = QuadMirrorUniforms {
            amount,
            _pad: [0.0; 3],
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "QuadMirror Pass",
            ctx.width,
            ctx.height,
        );
    }
}
