use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use super::compute_blit_helper::ComputeBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StrobeUniforms {
    amount: f32,
    rate: f32,
    mode: u32,      // 0=Opacity(black), 1=White, 2=Gain
    beat: f32,
}

/// NoteRates lookup table mapping param index to strobes-per-beat.
/// Unity ref: StrobeFX.cs lines 12-27
const NOTE_RATES: [f32; 9] = [0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0];

/// Strobe effect — beat-synced square wave flash.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct StrobeFX {
    helper: ComputeBlitHelper,
}

impl StrobeFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_strobe.wgsl"),
                "Strobe",
            ),
        }
    }
}

impl PostProcessEffect for StrobeFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::STROBE
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
        // Map rate index through NoteRates lookup table (Unity: StrobeFX.cs lines 24-26)
        let rate_idx = p.get(1).copied().unwrap_or(6.0).round().max(0.0) as usize;
        let rate = NOTE_RATES[rate_idx.min(NOTE_RATES.len() - 1)];
        let uniforms = StrobeUniforms {
            amount: p.first().copied().unwrap_or(0.0),
            rate,
            mode: (p.get(2).copied().unwrap_or(0.0).round() as u32).min(2),
            beat: ctx.beat,
        };

        self.helper.dispatch(
            gpu,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Strobe Pass",
            ctx.width, ctx.height,
        );
    }
}
