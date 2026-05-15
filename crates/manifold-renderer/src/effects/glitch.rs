use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::Glitch;
use crate::node_graph::{ParamBinding, ParamConvert, ParamTarget, SkipMode};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use std::borrow::Cow;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::GLITCH,
        display_name: "Glitch",
        category: "Filmic",
        available: true,
        osc_prefix: "glitch",
        legacy_discriminant: Some(32),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
            ParamSpec::whole("block", "Block", 4.0, 64.0, 16.0, "BlockSize"),
            ParamSpec::continuous("rgb_shift", "RGB Shift", 0.0, 0.05, 0.01, "F2", "RGBShift"),
            ParamSpec::continuous("scanline", "Scanline", 0.0, 1.0, 0.3, "F2", "Scanline"),
            ParamSpec::continuous("speed", "Speed", 0.1, 10.0, 2.0, "F2", "Speed"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::GLITCH,
        create: |device| Box::new(GlitchFX::new(device)),
    }
}

crate::atomic_chain_spec! {
    type_id: EffectTypeId::GLITCH,
    primitive: Glitch,
    handle: "glitch",
    bindings: &[
        ParamBinding {
            id: Cow::Borrowed("amount"),
            spec: ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
            target: ParamTarget::HandleNode { handle: "glitch", param: "amount" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("block"),
            spec: ParamSpec::whole("block", "Block", 4.0, 64.0, 16.0, "BlockSize"),
            target: ParamTarget::HandleNode { handle: "glitch", param: "block_size" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("rgb_shift"),
            spec: ParamSpec::continuous("rgb_shift", "RGB Shift", 0.0, 0.05, 0.01, "F2", "RGBShift"),
            target: ParamTarget::HandleNode { handle: "glitch", param: "rgb_shift" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("scanline"),
            spec: ParamSpec::continuous("scanline", "Scanline", 0.0, 1.0, 0.3, "F2", "Scanline"),
            target: ParamTarget::HandleNode { handle: "glitch", param: "scanline" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("speed"),
            spec: ParamSpec::continuous("speed", "Speed", 0.1, 10.0, 2.0, "F2", "Speed"),
            target: ParamTarget::HandleNode { handle: "glitch", param: "speed" },
            convert: ParamConvert::Float,
        },
        // `time` is a ctx-driven param — populated by
        // `apply_ctx_params_at` each frame from `EffectContext::time`.
    ],
    skip: SkipMode::OnZero { param_id: "amount" },
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlitchUniforms {
    amount: f32,
    block_size: f32,
    rgb_shift: f32,
    scanline: f32, // GlitchFX.cs:16 — _Scanline
    speed: f32,
    time: f32,
    resolution_x: f32,
    resolution_y: f32,
}

/// Glitch effect — block displacement, scanline jitter, RGB channel split.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct GlitchFX {
    helper: ComputeBlitHelper,
}

impl GlitchFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_glitch.wgsl"),
                "Glitch",
            ),
        }
    }
}

impl PostProcessEffect for GlitchFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::GLITCH
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        // GlitchFX.cs:13-18 — read all 5 params in Unity order
        let p = &fx.param_values;
        let uniforms = GlitchUniforms {
            amount: p.first().map(|pv| pv.value).unwrap_or(0.0), // line 13: _Amount
            block_size: p.get(1).map(|pv| pv.value).unwrap_or(16.0).max(4.0), // line 14: _BlockSize
            rgb_shift: p.get(2).map(|pv| pv.value).unwrap_or(0.01), // line 15: _RGBShift
            scanline: p.get(3).map(|pv| pv.value).unwrap_or(0.3), // line 16: _Scanline
            speed: p.get(4).map(|pv| pv.value).unwrap_or(2.0),   // line 17: _Speed
            time: ctx.time,                                      // line 18: Time.time
            resolution_x: ctx.output_width as f32,
            resolution_y: ctx.output_height as f32,
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "Glitch Pass",
            ctx.width,
            ctx.height,
        );
    }
}
