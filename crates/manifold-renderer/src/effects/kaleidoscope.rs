use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct KaleidoscopeUniforms {
    amount: f32,
    segments: f32,
    _pad0: f32,
    _pad1: f32,
}

/// Kaleidoscope effect — polar-coordinate segment mirroring.
pub struct KaleidoscopeFX {
    helper: SimpleBlitHelper,
}

impl KaleidoscopeFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/fx_kaleidoscope.wgsl"),
                "Kaleidoscope",
                std::mem::size_of::<KaleidoscopeUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for KaleidoscopeFX {
    fn effect_type(&self) -> EffectType {
        EffectType::Kaleidoscope
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
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let p = &fx.param_values;
        let uniforms = KaleidoscopeUniforms {
            amount: p.first().copied().unwrap_or(0.0),
            segments: p.get(1).copied().unwrap_or(6.0).max(2.0),
            _pad0: 0.0,
            _pad1: 0.0,
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Kaleidoscope Pass",
            ctx.width, ctx.height,
            profiler,
        );
    }
}
