use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::KaleidoFold;
use crate::node_graph::{ParamBinding, ParamConvert, ParamTarget, SkipMode};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use std::borrow::Cow;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::KALEIDOSCOPE,
        display_name: "Kaleidoscope",
        category: "Post-Process",
        available: true,
        osc_prefix: "kaleidoscope",
        legacy_discriminant: Some(14),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
            ParamSpec::whole("segs", "Segs", 2.0, 16.0, 6.0, "Segments"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::KALEIDOSCOPE,
        create: |device| Box::new(KaleidoscopeFX::new(device)),
    }
}

crate::atomic_chain_spec! {
    type_id: EffectTypeId::KALEIDOSCOPE,
    primitive: KaleidoFold,
    handle: "kaleidoscope",
    bindings: &[
        ParamBinding {
            id: Cow::Borrowed("amount"),
            spec: ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
            target: ParamTarget::HandleNode { handle: "kaleidoscope", param: "amount" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("segs"),
            spec: ParamSpec::whole("segs", "Segs", 2.0, 16.0, 6.0, "Segments"),
            target: ParamTarget::HandleNode { handle: "kaleidoscope", param: "segments" },
            convert: ParamConvert::Float,
        },
    ],
    skip: SkipMode::OnZero { param_id: "amount" },
}

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
            amount: p.first().map(|pv| pv.value).unwrap_or(0.0),
            segments: p.get(1).map(|pv| pv.value).unwrap_or(6.0).max(2.0),
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
