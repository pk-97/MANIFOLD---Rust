use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::Invert;
use crate::node_graph::{ParamBinding, ParamConvert, ParamTarget, SkipMode};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use std::borrow::Cow;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::INVERT_COLORS,
        display_name: "Invert",
        category: "Color",
        available: true,
        osc_prefix: "invert",
        legacy_discriminant: Some(1),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::INVERT_COLORS,
        create: |device| Box::new(InvertColorsFX::new(device)),
    }
}

crate::atomic_chain_spec! {
    type_id: EffectTypeId::INVERT_COLORS,
    primitive: Invert,
    handle: "invert",
    bindings: &[
        ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,
            target: ParamTarget::HandleNode { handle: "invert", param: "intensity" },
            convert: ParamConvert::Float,
        },
    ],
    skip: SkipMode::OnZero { param_id: "amount" },
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InvertUniforms {
    intensity: f32,
    _pad: [f32; 3],
}

/// InvertColors effect — `1.0 - rgb`. Simplest possible effect for smoke testing.
pub struct InvertColorsFX {
    helper: ComputeBlitHelper,
}

impl InvertColorsFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
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
        let intensity = fx.param_values.first().map(|p| p.value).unwrap_or(1.0);
        let uniforms = InvertUniforms {
            intensity,
            _pad: [0.0; 3],
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "InvertColors Pass",
            ctx.width,
            ctx.height,
        );
    }
}
