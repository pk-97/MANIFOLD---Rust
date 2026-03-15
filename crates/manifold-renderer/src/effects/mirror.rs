use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MirrorUniforms {
    mode: u32,
    _pad: [f32; 3],
}

/// Mirror effect — horizontal, vertical, or quad mirror.
pub struct MirrorFX {
    helper: SimpleBlitHelper,
}

impl MirrorFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/mirror.wgsl"),
                "Mirror",
                std::mem::size_of::<MirrorUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for MirrorFX {
    fn effect_type(&self) -> EffectType {
        EffectType::Mirror
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
        // param[0]: mode — 0=horizontal, 1=vertical, 2=quad
        let mode = fx.param_values.first().copied().unwrap_or(0.0) as u32;
        let uniforms = MirrorUniforms {
            mode: mode.min(2),
            _pad: [0.0; 3],
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Mirror Pass",
        );
    }
}
