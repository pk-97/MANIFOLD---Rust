use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DitherUniforms {
    amount: f32,
    algorithm: u32,    // 0=Bayer,1=Halftone,2=Lines,3=CrossHatch,4=Noise,5=Diamond
    resolution_x: f32,
    resolution_y: f32,
}

/// Dither effect — 6 dithering algorithms with luminance-preserving quantization.
pub struct DitherFX {
    helper: SimpleBlitHelper,
}

impl DitherFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/fx_dither.wgsl"),
                "Dither",
                std::mem::size_of::<DitherUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for DitherFX {
    fn effect_type(&self) -> EffectType {
        EffectType::Dither
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
        let uniforms = DitherUniforms {
            amount: p.first().copied().unwrap_or(0.0),
            algorithm: (p.get(1).copied().unwrap_or(0.0).round() as u32).min(5),
            resolution_x: ctx.width as f32,
            resolution_y: ctx.height as f32,
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Dither Pass",
            profiler,
        );
    }
}
