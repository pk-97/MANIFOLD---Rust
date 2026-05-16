// Mechanical port of Unity ColorGradeFX.cs.
// 9 parameters, single pass, K-matrix HSV, colorize pipeline.

use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::ColorGrade;
use crate::node_graph::{ParamBinding, ParamConvert, ParamTarget, SkipMode};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use std::borrow::Cow;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::COLOR_GRADE,
        display_name: "Color Grade",
        category: "Post-Process",
        available: true,
        osc_prefix: "colorGrade",
        legacy_discriminant: Some(28),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
            ParamSpec::continuous("gain", "Gain", 0.0, 2.0, 1.0, "F2", "Gain"),
            ParamSpec::continuous("sat", "Sat", 0.0, 2.0, 1.0, "F2", "Saturation"),
            ParamSpec::continuous("hue", "Hue", -180.0, 180.0, 0.0, "F2", "Hue"),
            ParamSpec::continuous("contrast", "Contrast", 0.0, 2.0, 1.0, "F2", "Contrast"),
            ParamSpec::continuous("colorize", "Colorize", 0.0, 1.0, 0.0, "F2", "Colorize"),
            ParamSpec::continuous("tint_hue", "TintHue", 0.0, 360.0, 0.0, "F2", "TintHue"),
            ParamSpec::continuous("tint_sat", "TintSat", 0.0, 2.0, 1.0, "F2", "TintSaturation"),
            ParamSpec::continuous("focus", "Focus", 0.0, 1.0, 0.75, "F2", "ColorizeFocus"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::COLOR_GRADE,
        create: |device| Box::new(ColorGradeFX::new(device)),
    }
}

crate::atomic_chain_spec! {
    type_id: EffectTypeId::COLOR_GRADE,
    primitive: ColorGrade,
    handle: "color_grade",
    bindings: &[
        ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,
            target: ParamTarget::HandleNode { handle: "color_grade", param: "amount" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("gain"),
            label: "Gain",
            default_value: 1.0,
            target: ParamTarget::HandleNode { handle: "color_grade", param: "gain" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("sat"),
            label: "Sat",
            default_value: 1.0,
            target: ParamTarget::HandleNode { handle: "color_grade", param: "saturation" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("hue"),
            label: "Hue",
            default_value: 0.0,
            target: ParamTarget::HandleNode { handle: "color_grade", param: "hue" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("contrast"),
            label: "Contrast",
            default_value: 1.0,
            target: ParamTarget::HandleNode { handle: "color_grade", param: "contrast" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("colorize"),
            label: "Colorize",
            default_value: 0.0,
            target: ParamTarget::HandleNode { handle: "color_grade", param: "colorize" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("tint_hue"),
            label: "TintHue",
            default_value: 0.0,
            target: ParamTarget::HandleNode { handle: "color_grade", param: "colorize_hue" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("tint_sat"),
            label: "TintSat",
            default_value: 1.0,
            target: ParamTarget::HandleNode { handle: "color_grade", param: "colorize_saturation" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("focus"),
            label: "Focus",
            default_value: 0.75,
            target: ParamTarget::HandleNode { handle: "color_grade", param: "colorize_focus" },
            convert: ParamConvert::Float,
        },
    ],
    skip: SkipMode::OnZero { param_id: "amount" },
}

// ColorGradeEffect.shader Properties (lines 6-14)
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorGradeUniforms {
    amount: f32,              // _Amount
    gain: f32,                // _Gain
    saturation: f32,          // _Saturation
    hue: f32,                 // _Hue (degrees, -180..180)
    contrast: f32,            // _Contrast
    colorize: f32,            // _Colorize
    colorize_hue: f32,        // _ColorizeHue (degrees, 0..360)
    colorize_saturation: f32, // _ColorizeSaturation
    colorize_focus: f32,      // _ColorizeFocus
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// ColorGrade effect — gain, saturation, hue shift, contrast, colorize tinting.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct ColorGradeFX {
    helper: ComputeBlitHelper,
}

impl ColorGradeFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/color_grade.wgsl"),
                "ColorGrade",
            ),
        }
    }
}

impl PostProcessEffect for ColorGradeFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::COLOR_GRADE
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        // ColorGradeFX.cs:31-39 — read all 9 params in Unity order
        let p = &fx.param_values;
        let amount = p.first().map(|pv| pv.value).unwrap_or(0.0);
        // ShouldSkip handles the identity check at the chain level now.

        let uniforms = ColorGradeUniforms {
            amount,                                                          // line 31
            gain: p.get(1).map(|pv| pv.value).unwrap_or(1.0),                // line 32
            saturation: p.get(2).map(|pv| pv.value).unwrap_or(1.0),          // line 33
            hue: p.get(3).map(|pv| pv.value).unwrap_or(0.0),                 // line 34
            contrast: p.get(4).map(|pv| pv.value).unwrap_or(1.0),            // line 35
            colorize: p.get(5).map(|pv| pv.value).unwrap_or(0.0),            // line 36
            colorize_hue: p.get(6).map(|pv| pv.value).unwrap_or(0.0),        // line 37
            colorize_saturation: p.get(7).map(|pv| pv.value).unwrap_or(1.0), // line 38: ParamCount > 7 ? GetParam(7) : 1f
            colorize_focus: p.get(8).map(|pv| pv.value).unwrap_or(0.75), // line 39: ParamCount > 8 ? GetParam(8) : 0.75f
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "ColorGrade Pass",
            ctx.width,
            ctx.height,
        );
    }
}
