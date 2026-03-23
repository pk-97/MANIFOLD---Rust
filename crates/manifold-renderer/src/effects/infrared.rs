use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InfraredUniforms {
    amount: f32,
    palette: f32,
    contrast: f32,
    noise: f32,
    scanline: f32,
    hot_spot: f32,
    time: f32,
    texel_size_x: f32,  // 1/width  (_MainTex_TexelSize.x)
    texel_size_y: f32,  // 1/height (_MainTex_TexelSize.y)
    texel_size_z: f32,  // width    (_MainTex_TexelSize.z)
    texel_size_w: f32,  // height   (_MainTex_TexelSize.w)
    _pad0: f32,
}

/// Infrared / thermal vision effect.
/// Unity ref: InfraredFX.cs / InfraredEffect.shader
pub struct InfraredFX {
    helper: SimpleBlitHelper,
}

impl InfraredFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/fx_infrared.wgsl"),
                "Infrared",
                std::mem::size_of::<InfraredUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for InfraredFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::INFRARED
    }

    fn should_skip(&self, fx: &EffectInstance) -> bool {
        fx.param_values.first().copied().unwrap_or(0.0) <= 0.0
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
        let width = ctx.width as f32;
        let height = ctx.height as f32;
        let uniforms = InfraredUniforms {
            amount:      p.first().copied().unwrap_or(0.0),
            palette:     p.get(1).copied().unwrap_or(0.0),
            contrast:    p.get(2).copied().unwrap_or(1.0),
            noise:       p.get(3).copied().unwrap_or(0.15),
            scanline:    p.get(4).copied().unwrap_or(0.0),
            hot_spot:    p.get(5).copied().unwrap_or(0.0),
            time:        ctx.time,
            texel_size_x: 1.0 / width,
            texel_size_y: 1.0 / height,
            texel_size_z: width,
            texel_size_w: height,
            _pad0: 0.0,
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Infrared Pass",
            ctx.width, ctx.height,
            profiler,
        );
    }
}
