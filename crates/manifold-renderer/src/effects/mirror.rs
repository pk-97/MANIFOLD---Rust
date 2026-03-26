use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use super::compute_blit_helper::ComputeBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MirrorUniforms {
    amount: f32,   // MirrorFX.cs:13 — _Amount
    mode: u32,     // MirrorFX.cs:14 — _Mode
    _pad: [f32; 2],
}

/// Mirror effect — horizontal, vertical, or quad mirror.
/// Uses compute dispatch to bypass Metal TBDR tile overhead.
pub struct MirrorFX {
    helper: ComputeBlitHelper,
}

impl MirrorFX {
    pub fn new(
        device: &wgpu::Device,
        hal_ctx: Option<&crate::hal_context::HalContext>,
        #[cfg(target_os = "macos")] native_device: Option<&manifold_gpu::GpuDevice>,
    ) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/mirror_compute.wgsl"),
                "Mirror",
                std::mem::size_of::<MirrorUniforms>() as u64,
                hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            ),
        }
    }
}

impl PostProcessEffect for MirrorFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::MIRROR
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
        let amount = p.first().copied().unwrap_or(1.0);
        let mode = p.get(1).copied().unwrap_or(0.0).round() as u32;
        let uniforms = MirrorUniforms {
            amount,
            mode: mode.min(2),
            _pad: [0.0; 2],
        };

        self.helper.dispatch(
            gpu,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Mirror Pass",
            ctx.width, ctx.height,
            profiler,
        );
    }
}
