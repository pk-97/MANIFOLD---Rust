use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::ChromaticOffset;
use crate::node_graph::{ParamBinding, ParamConvert, ParamTarget, SkipMode};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use std::borrow::Cow;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::CHROMATIC_ABERRATION,
        display_name: "Chromatic Aberration",
        category: "Filmic",
        available: true,
        osc_prefix: "chromAb",
        legacy_discriminant: Some(30),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
            ParamSpec::continuous("offset", "Offset", 0.0, 0.05, 0.01, "F2", "Offset"),
            ParamSpec::whole_labels("mode", "Mode", 0.0, 1.0, 0.0, &["Radial", "Linear"], "Mode"),
            ParamSpec::continuous("angle", "Angle", 0.0, 360.0, 0.0, "F2", "Angle"),
            ParamSpec::continuous("falloff", "Falloff", 0.0, 1.0, 0.5, "F2", "Falloff"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::CHROMATIC_ABERRATION,
        create: |device| Box::new(ChromaticAberrationFX::new(device)),
    }
}

crate::atomic_chain_spec! {
    type_id: EffectTypeId::CHROMATIC_ABERRATION,
    primitive: ChromaticOffset,
    handle: "chromatic",
    bindings: &[
        ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,
            target: ParamTarget::HandleNode { handle: "chromatic", param: "amount" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("offset"),
            label: "Offset",
            default_value: 0.01,
            target: ParamTarget::HandleNode { handle: "chromatic", param: "offset" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("mode"),
            label: "Mode",
            default_value: 0.0,
            target: ParamTarget::HandleNode { handle: "chromatic", param: "mode" },
            convert: ParamConvert::EnumRound,
        },
        ParamBinding {
            id: Cow::Borrowed("angle"),
            label: "Angle",
            default_value: 0.0,
            target: ParamTarget::HandleNode { handle: "chromatic", param: "angle" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("falloff"),
            label: "Falloff",
            default_value: 0.5,
            target: ParamTarget::HandleNode { handle: "chromatic", param: "falloff" },
            convert: ParamConvert::Float,
        },
    ],
    skip: SkipMode::OnZero { param_id: "amount" },
}

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
        let amount = p.first().map(|pv| pv.value).unwrap_or(0.0); // line 13: _Amount
        let offset = p.get(1).map(|pv| pv.value).unwrap_or(0.01); // line 14: _Offset (independent)
        let mode = p.get(2).map(|pv| pv.value).unwrap_or(0.0).round() as u32; // line 15: Mathf.Round(_Mode)
        let angle = p.get(3).map(|pv| pv.value).unwrap_or(0.0); // line 16: _Angle
        let falloff = p.get(4).map(|pv| pv.value).unwrap_or(0.5); // line 17: _Falloff

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
