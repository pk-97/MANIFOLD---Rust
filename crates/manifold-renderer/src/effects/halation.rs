use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HalationUniforms {
    amount: f32,
    threshold: f32,
    spread: f32,
    hue: f32,
    resolution_x: f32,
    resolution_y: f32,
    _pad0: f32,
    _pad1: f32,
}

/// Halation effect — bright-area extraction with tinted blur, additive composite.
pub struct HalationFX {
    helper: SimpleBlitHelper,
}

impl HalationFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/fx_halation.wgsl"),
                "Halation",
                std::mem::size_of::<HalationUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for HalationFX {
    fn effect_type(&self) -> EffectType {
        EffectType::Halation
    }

    fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        let p = &fx.param_values;
        let uniforms = HalationUniforms {
            amount: p.first().copied().unwrap_or(0.0),
            threshold: p.get(1).copied().unwrap_or(0.5),
            spread: p.get(2).copied().unwrap_or(0.5),
            hue: p.get(3).copied().unwrap_or(0.05), // Default warm orange
            resolution_x: ctx.width as f32,
            resolution_y: ctx.height as f32,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Halation Pass",
        );
    }
}
