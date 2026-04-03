use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChromaticAberrationUniforms {
    amount: f32,
    mode: u32, // 0=Radial, 1=Linear
    angle: f32,
    falloff: f32,
    offset: f32,
    _pad: [f32; 3],
}

/// ChromaticAberration effect — radial or linear RGB channel separation.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct ChromaticAberrationFX {
    helper: ComputeBlitHelper,
}

impl ChromaticAberrationFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_chromatic_aberration.wgsl"),
                "ChromaticAberration",
            ),
        }
    }
}

impl PostProcessEffect for ChromaticAberrationFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::CHROMATIC_ABERRATION
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        // ChromaticAberrationFX.cs:13-17 — read all 5 params in Unity order
        let p = &fx.param_values;
        let amount = p.first().copied().unwrap_or(0.0); // line 13: _Amount
        let offset = p.get(1).copied().unwrap_or(0.01); // line 14: _Offset (independent)
        let mode = p.get(2).copied().unwrap_or(0.0).round() as u32; // line 15: Mathf.Round(_Mode)
        let angle = p.get(3).copied().unwrap_or(0.0); // line 16: _Angle
        let falloff = p.get(4).copied().unwrap_or(0.5); // line 17: _Falloff

        let uniforms = ChromaticAberrationUniforms {
            amount,
            mode: mode.min(1),
            angle,
            falloff,
            offset,
            _pad: [0.0; 3],
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "ChromaticAberration Pass",
            ctx.width,
            ctx.height,
        );
    }
}
