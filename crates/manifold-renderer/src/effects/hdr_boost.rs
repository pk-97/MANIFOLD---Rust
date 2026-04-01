// HDR Boost — sharp highlight extraction + gain, no blur.
// Same soft-knee threshold as bloom but without any blur passes.
// Single pass via ComputeBlitHelper.

use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use super::compute_blit_helper::ComputeBlitHelper;

const EPSILON: f32 = 0.001;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HdrBoostUniforms {
    amount: f32,
    gain: f32,
    threshold: f32,
    knee: f32,
}

pub struct HdrBoostFX {
    helper: ComputeBlitHelper,
}

impl HdrBoostFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/hdr_boost_compute.wgsl"),
                "HdrBoost",
            ),
        }
    }
}

impl PostProcessEffect for HdrBoostFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::HDR_BOOST
    }

    fn should_skip(&self, fx: &EffectInstance) -> bool {
        let p = &fx.param_values;
        let amount = p.first().copied().unwrap_or(0.0);
        if amount <= 0.0 {
            return true;
        }
        let gain = p.get(1).copied().unwrap_or(1.5);
        gain.abs() < EPSILON
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
        let uniforms = HdrBoostUniforms {
            amount: p.first().copied().unwrap_or(0.0),
            gain: p.get(1).copied().unwrap_or(1.5),
            threshold: p.get(2).copied().unwrap_or(0.15),
            knee: p.get(3).copied().unwrap_or(0.3),
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "HdrBoost Pass",
            ctx.width,
            ctx.height,
        );
    }
}
