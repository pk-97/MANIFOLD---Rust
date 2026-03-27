use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use super::fragment_blit_helper::FragmentBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InvertUniforms {
    intensity: f32,
    _pad: [f32; 3],
}

/// InvertColors effect — `1.0 - rgb`. Simplest possible effect for smoke testing.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct InvertColorsFX {
    helper: FragmentBlitHelper,
}

impl InvertColorsFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: FragmentBlitHelper::new(
                device,
                include_str!("shaders/invert_colors.wgsl"),
                "InvertColors",
            ),
        }
    }
}

impl PostProcessEffect for InvertColorsFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::INVERT_COLORS
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        let intensity = fx.param_values.first().copied().unwrap_or(1.0);
        let uniforms = InvertUniforms {
            intensity,
            _pad: [0.0; 3],
        };

        self.helper.dispatch(
            gpu,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "InvertColors Pass",
            ctx.width, ctx.height,
        );
    }
}
