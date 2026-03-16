use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CrtUniforms {
    amount: f32,
    scanlines: f32,
    glow: f32,
    curvature: f32,
    style: f32,
    resolution_x: f32,
    resolution_y: f32,
    _pad: f32,
}

/// CRT effect — barrel distortion, scanlines, RGB phosphor mask, glow, vignette.
pub struct CrtFX {
    helper: SimpleBlitHelper,
}

impl CrtFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/fx_crt.wgsl"),
                "CRT",
                std::mem::size_of::<CrtUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for CrtFX {
    fn effect_type(&self) -> EffectType {
        EffectType::CRT
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
        let uniforms = CrtUniforms {
            amount: p.first().copied().unwrap_or(0.0),
            scanlines: p.get(1).copied().unwrap_or(0.5),
            glow: p.get(2).copied().unwrap_or(0.3),
            curvature: p.get(3).copied().unwrap_or(0.2),
            style: p.get(4).copied().unwrap_or(0.5),
            resolution_x: ctx.width as f32,
            resolution_y: ctx.height as f32,
            _pad: 0.0,
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "CRT Pass",
        );
    }
}
