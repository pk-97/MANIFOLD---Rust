use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MirrorUniforms {
    amount: f32, // MirrorFX.cs:13 — _Amount
    mode: u32,   // MirrorFX.cs:14 — _Mode
    _pad: [f32; 2],
}

/// Mirror effect — horizontal, vertical, or quad mirror.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct MirrorFX {
    helper: ComputeBlitHelper,
}

impl MirrorFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(device, include_str!("shaders/mirror.wgsl"), "Mirror"),
        }
    }
}

impl PostProcessEffect for MirrorFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::MIRROR
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
        let amount = p.first().copied().unwrap_or(1.0);
        let mode = p.get(1).copied().unwrap_or(0.0).round() as u32;
        let uniforms = MirrorUniforms {
            amount,
            mode: mode.min(2),
            _pad: [0.0; 2],
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "Mirror Pass",
            ctx.width,
            ctx.height,
        );
    }
}
