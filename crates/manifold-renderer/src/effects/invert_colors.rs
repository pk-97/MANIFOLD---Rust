use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use super::compute_blit_helper::ComputeBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InvertUniforms {
    intensity: f32,
    _pad: [f32; 3],
}

/// InvertColors effect — `1.0 - rgb`. Simplest possible effect for smoke testing.
pub struct InvertColorsFX {
    helper: ComputeBlitHelper,
}

impl InvertColorsFX {
    pub fn new(
        device: &wgpu::Device,
        hal_ctx: Option<&crate::hal_context::HalContext>,
        #[cfg(target_os = "macos")] native_device: Option<&manifold_gpu::GpuDevice>,
    ) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/invert_colors_compute.wgsl"),
                "InvertColors",
                std::mem::size_of::<InvertUniforms>() as u64,
                hal_ctx,
                #[cfg(target_os = "macos")] native_device,
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
        gpu: &mut GpuEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        _target_texture: &wgpu::Texture,
        fx: &EffectInstance,
        ctx: &EffectContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let intensity = fx.param_values.first().copied().unwrap_or(1.0);
        let uniforms = InvertUniforms {
            intensity,
            _pad: [0.0; 3],
        };

        self.helper.dispatch(
            gpu,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "InvertColors Pass",
            ctx.width, ctx.height,
            profiler,
        );
    }
}
