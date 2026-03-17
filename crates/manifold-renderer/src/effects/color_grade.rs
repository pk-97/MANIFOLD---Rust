// Mechanical port of Unity ColorGradeFX.cs.
// 9 parameters, single pass, K-matrix HSV, colorize pipeline.

use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

// ColorGradeEffect.shader Properties (lines 6-14)
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorGradeUniforms {
    amount: f32,                // _Amount
    gain: f32,                  // _Gain
    saturation: f32,            // _Saturation
    hue: f32,                   // _Hue (degrees, -180..180)
    contrast: f32,              // _Contrast
    colorize: f32,              // _Colorize
    colorize_hue: f32,          // _ColorizeHue (degrees, 0..360)
    colorize_saturation: f32,   // _ColorizeSaturation
    colorize_focus: f32,        // _ColorizeFocus
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// ColorGrade effect — gain, saturation, hue shift, contrast, colorize tinting.
pub struct ColorGradeFX {
    helper: SimpleBlitHelper,
}

impl ColorGradeFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/color_grade.wgsl"),
                "ColorGrade",
                std::mem::size_of::<ColorGradeUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for ColorGradeFX {
    fn effect_type(&self) -> EffectType {
        EffectType::ColorGrade
    }

    fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        fx: &EffectInstance,
        _ctx: &EffectContext,
    ) {
        // ColorGradeFX.cs:31-39 — read all 9 params in Unity order
        let p = &fx.param_values;
        let amount = p.first().copied().unwrap_or(0.0);

        // ColorGradeFX.cs:13-26 — ShouldSkip optimization
        // Skip when amount <= 0 or all processing params are at identity
        if amount <= 0.0 {
            // Passthrough — SimpleBlitHelper doesn't support skip, so just apply with amount=0
            // The shader's final lerp(src, c, 0) = src
        }

        let uniforms = ColorGradeUniforms {
            amount,                                                        // line 31
            gain: p.get(1).copied().unwrap_or(1.0),                       // line 32
            saturation: p.get(2).copied().unwrap_or(1.0),                 // line 33
            hue: p.get(3).copied().unwrap_or(0.0),                        // line 34
            contrast: p.get(4).copied().unwrap_or(1.0),                   // line 35
            colorize: p.get(5).copied().unwrap_or(0.0),                   // line 36
            colorize_hue: p.get(6).copied().unwrap_or(0.0),               // line 37
            colorize_saturation: p.get(7).copied().unwrap_or(1.0),        // line 38: ParamCount > 7 ? GetParam(7) : 1f
            colorize_focus: p.get(8).copied().unwrap_or(0.75),            // line 39: ParamCount > 8 ? GetParam(8) : 0.75f
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "ColorGrade Pass",
        );
    }
}
