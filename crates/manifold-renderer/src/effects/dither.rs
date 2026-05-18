use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::{EffectAliasMetadata, EffectMetadata};
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::DITHER,
        display_name: "Dither",
        category: "Color",
        available: true,
        osc_prefix: "dither",
        legacy_discriminant: Some(18),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),
            ParamSpec::whole_labels("pattern", "Pattern", 0.0, 5.0, 0.0, &["Bayer", "Halftone", "Lines", "X-Hatch", "Noise", "Diamond"], "Algorithm"),
        ],
    }
}

inventory::submit! {
    EffectAliasMetadata {
        id: EffectTypeId::DITHER,
        aliases: &[("algo", Some("pattern"))],
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DitherUniforms {
    amount: f32,
    algorithm: u32, // 0=Bayer,1=Halftone,2=Lines,3=CrossHatch,4=Noise,5=Diamond
    resolution_x: f32,
    resolution_y: f32,
}

/// Dither effect — 6 dithering algorithms with luminance-preserving quantization.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct DitherFX {
    helper: ComputeBlitHelper,
}

impl DitherFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
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
            amount: p.first().map(|pv| pv.value).unwrap_or(0.0),
            algorithm: (p.get(1).map(|pv| pv.value).unwrap_or(0.0).round() as u32).min(5),
            resolution_x: ctx.output_width as f32,
            resolution_y: ctx.output_height as f32,
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "Dither Pass",
            ctx.width,
            ctx.height,
        );
    }
}
