use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use super::compute_blit_helper::ComputeBlitHelper;

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
    helper: ComputeBlitHelper,
}

impl DitherFX {
    pub fn new(
        device: &wgpu::Device,
        hal_ctx: Option<&crate::hal_context::HalContext>,
        #[cfg(target_os = "macos")] native_device: Option<&manifold_gpu::GpuDevice>,
    ) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_dither_compute.wgsl"),
                "Dither",
                std::mem::size_of::<DitherUniforms>() as u64,
                hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            ),
        }
    }
}

impl PostProcessEffect for DitherFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::DITHER
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        _target_texture: &wgpu::Texture,
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

        self.helper.dispatch(
            gpu,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Dither Pass",
            ctx.width, ctx.height,
            profiler,
        );
    }
}
