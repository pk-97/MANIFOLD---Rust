use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FilmGrainUniforms {
    amount: f32,
    grain_size: f32,
    luma_weight: f32,
    color_grain: f32,
    time: f32,
    resolution_x: f32,
    resolution_y: f32,
    _pad0: f32,
}

/// FilmGrain effect — hash-based temporal noise with luma-weighted intensity.
pub struct FilmGrainFX {
    helper: SimpleBlitHelper,
}

impl FilmGrainFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/fx_film_grain.wgsl"),
                "FilmGrain",
                std::mem::size_of::<FilmGrainUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for FilmGrainFX {
    fn effect_type(&self) -> EffectType {
        EffectType::FilmGrain
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
        let uniforms = FilmGrainUniforms {
            amount: p.first().copied().unwrap_or(0.0),
            grain_size: p.get(1).copied().unwrap_or(1.5).max(0.5),
            luma_weight: p.get(2).copied().unwrap_or(0.5),
            color_grain: p.get(3).copied().unwrap_or(0.0),
            time: ctx.time,
            resolution_x: ctx.width as f32,
            resolution_y: ctx.height as f32,
            _pad0: 0.0,
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "FilmGrain Pass",
            profiler,
        );
    }
}
