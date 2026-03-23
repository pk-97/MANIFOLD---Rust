use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InvertUniforms {
    intensity: f32,
    _pad: [f32; 3],
}

/// InvertColors effect — `1.0 - rgb`. Simplest possible effect for smoke testing.
pub struct InvertColorsFX {
    helper: SimpleBlitHelper,
}

impl InvertColorsFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/invert_colors.wgsl"),
                "InvertColors",
                std::mem::size_of::<InvertUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for InvertColorsFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::INVERT_COLORS
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
        let intensity = fx.param_values.first().copied().unwrap_or(1.0);
        let uniforms = InvertUniforms {
            intensity,
            _pad: [0.0; 3],
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "InvertColors Pass",
            ctx.width, ctx.height,
            profiler,
        );
    }
}
