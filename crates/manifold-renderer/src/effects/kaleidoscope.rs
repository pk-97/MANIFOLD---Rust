use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct KaleidoscopeUniforms {
    amount: f32,
    segments: f32,
    _pad0: f32,
    _pad1: f32,
}

/// Kaleidoscope effect — polar-coordinate segment mirroring.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct KaleidoscopeFX {
    helper: ComputeBlitHelper,
}

impl KaleidoscopeFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_kaleidoscope.wgsl"),
                "Kaleidoscope",
            ),
        }
    }
}

impl PostProcessEffect for KaleidoscopeFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::KALEIDOSCOPE
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
        let uniforms = KaleidoscopeUniforms {
            amount: p.first().copied().unwrap_or(0.0),
            segments: p.get(1).copied().unwrap_or(6.0).max(2.0),
            _pad0: 0.0,
            _pad1: 0.0,
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "Kaleidoscope Pass",
            ctx.width,
            ctx.height,
        );
    }
}
