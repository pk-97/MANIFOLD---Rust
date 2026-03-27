use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use super::fragment_blit_helper::FragmentBlitHelper;

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
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
/// Unity ref: InfraredFX.cs / InfraredEffect.shader
pub struct InfraredFX {
    helper: FragmentBlitHelper,
}

impl InfraredFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: FragmentBlitHelper::new(
                device,
                include_str!("shaders/fx_infrared.wgsl"),
                "Infrared",
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
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
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

        self.helper.dispatch(
            gpu,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Infrared Pass",
            ctx.width, ctx.height,
        );
    }
}
