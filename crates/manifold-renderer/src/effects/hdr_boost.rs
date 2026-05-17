// HDR Boost — sharp highlight extraction + gain, no blur.
// Same soft-knee threshold as bloom but without any blur passes.
// Single pass via ComputeBlitHelper.

use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::HighlightBoost;
use crate::node_graph::{ParamBinding, ParamConvert, ParamTarget, SkipMode};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use std::borrow::Cow;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::HDR_BOOST,
        display_name: "Highlight Boost",
        category: "Filmic",
        available: true,
        osc_prefix: "hdrBoost",
        legacy_discriminant: Some(41),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
            ParamSpec::continuous("gain", "Gain", 0.0, 5.0, 1.5, "F2", "Gain"),
            ParamSpec::continuous("thresh", "Threshold", 0.0, 1.0, 0.15, "F2", "Threshold"),
            ParamSpec::continuous("knee", "Knee", 0.0, 1.0, 0.3, "F2", "Knee"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::HDR_BOOST,
        create: |device| Box::new(HdrBoostFX::new(device)),
    }
}

crate::atomic_chain_spec! {
    type_id: EffectTypeId::HDR_BOOST,
    primitive: HighlightBoost,
    handle: "highlight_boost",
    bindings: &[
        ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,
            target: ParamTarget::HandleNode { handle: "highlight_boost", param: "amount" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("gain"),
            label: "Gain",
            default_value: 1.5,
            target: ParamTarget::HandleNode { handle: "highlight_boost", param: "gain" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("thresh"),
            label: "Threshold",
            default_value: 0.15,
            target: ParamTarget::HandleNode { handle: "highlight_boost", param: "threshold" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("knee"),
            label: "Knee",
            default_value: 0.3,
            target: ParamTarget::HandleNode { handle: "highlight_boost", param: "knee" },
            convert: ParamConvert::Float,
        },
    ],
    skip: SkipMode::OnZero { param_id: "amount" },
}

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
            amount: p.first().map(|pv| pv.value).unwrap_or(0.0),
            gain: p.get(1).map(|pv| pv.value).unwrap_or(1.5),
            threshold: p.get(2).map(|pv| pv.value).unwrap_or(0.15),
            knee: p.get(3).map(|pv| pv.value).unwrap_or(0.3),
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
