use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdgeStretchUniforms {
    amount: f32,
    source_width: f32,
    mode: u32,          // 0=Horizontal, 1=Vertical, 2=Both
    _pad: f32,
}

/// EdgeStretch effect — clamps UVs to a center strip, stretching edge pixels.
pub struct EdgeStretchFX {
    helper: SimpleBlitHelper,
}

impl EdgeStretchFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/fx_edge_stretch.wgsl"),
                "EdgeStretch",
                std::mem::size_of::<EdgeStretchUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for EdgeStretchFX {
    fn effect_type(&self) -> EffectType {
        EffectType::EdgeStretch
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
        let uniforms = EdgeStretchUniforms {
            amount: p.first().copied().unwrap_or(0.0),
            source_width: p.get(1).copied().unwrap_or(0.5625).clamp(0.1, 0.9),
            mode: (p.get(2).copied().unwrap_or(0.0) as u32).min(2),
            _pad: 0.0,
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "EdgeStretch Pass",
        );
    }
}
