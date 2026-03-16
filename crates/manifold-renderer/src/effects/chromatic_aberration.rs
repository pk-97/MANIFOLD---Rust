use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChromaticAberrationUniforms {
    amount: f32,
    mode: u32,       // 0=Radial, 1=Linear
    angle: f32,
    falloff: f32,
    offset: f32,
    _pad: [f32; 3],
}

/// ChromaticAberration effect — radial or linear RGB channel separation.
pub struct ChromaticAberrationFX {
    helper: SimpleBlitHelper,
}

impl ChromaticAberrationFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/fx_chromatic_aberration.wgsl"),
                "ChromaticAberration",
                std::mem::size_of::<ChromaticAberrationUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for ChromaticAberrationFX {
    fn effect_type(&self) -> EffectType {
        EffectType::ChromaticAberration
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
        let amount = p.first().copied().unwrap_or(0.0);
        let mode = p.get(1).copied().unwrap_or(0.0) as u32;
        let angle = p.get(2).copied().unwrap_or(0.0);
        let falloff = p.get(3).copied().unwrap_or(0.5);
        // Offset derived from amount: 0..1 -> 0..0.05
        let offset = amount * 0.05;

        let uniforms = ChromaticAberrationUniforms {
            amount,
            mode: mode.min(1),
            angle,
            falloff,
            offset,
            _pad: [0.0; 3],
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "ChromaticAberration Pass",
        );
    }
}
