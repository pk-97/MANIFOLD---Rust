use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorGradeUniforms {
    hue_shift: f32,
    saturation: f32,
    gain: f32,
    contrast: f32,
}

/// ColorGrade effect — HSV hue shift, saturation, gain, contrast.
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
        let p = &fx.param_values;
        let uniforms = ColorGradeUniforms {
            hue_shift: p.first().copied().unwrap_or(0.0),
            saturation: p.get(1).copied().unwrap_or(1.0),
            gain: p.get(2).copied().unwrap_or(1.0),
            contrast: p.get(3).copied().unwrap_or(1.0),
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "ColorGrade Pass",
        );
    }
}
