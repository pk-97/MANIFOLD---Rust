use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadMirrorUniforms {
    amount: f32,
    _pad: [f32; 3],
}

/// QuadMirror effect — mirrors UVs around center in both axes with crossfade.
pub struct QuadMirrorFX {
    helper: SimpleBlitHelper,
}

impl QuadMirrorFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/fx_quad_mirror.wgsl"),
                "QuadMirror",
                std::mem::size_of::<QuadMirrorUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for QuadMirrorFX {
    fn effect_type(&self) -> EffectType {
        EffectType::QuadMirror
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
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        // QuadMirrorFX.cs:13 — fx.GetParam(0), registry default 1.0
        let amount = fx.param_values.first().copied().unwrap_or(1.0);
        let uniforms = QuadMirrorUniforms {
            amount,
            _pad: [0.0; 3],
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "QuadMirror Pass",
            profiler,
        );
    }
}
