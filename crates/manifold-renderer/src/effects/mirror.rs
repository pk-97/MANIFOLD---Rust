use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MirrorUniforms {
    amount: f32,   // MirrorFX.cs:13 — _Amount
    mode: u32,     // MirrorFX.cs:14 — _Mode
    _pad: [f32; 2],
}

/// Mirror effect — horizontal, vertical, or quad mirror.
pub struct MirrorFX {
    helper: SimpleBlitHelper,
}

impl MirrorFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/mirror.wgsl"),
                "Mirror",
                std::mem::size_of::<MirrorUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for MirrorFX {
    fn effect_type(&self) -> EffectType {
        EffectType::Mirror
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
        // MirrorFX.cs:13-14 — GetParam(0)=Amount, Mathf.Round(GetParam(1))=Mode
        let p = &fx.param_values;
        let amount = p.first().copied().unwrap_or(1.0);
        let mode = p.get(1).copied().unwrap_or(0.0).round() as u32;
        let uniforms = MirrorUniforms {
            amount,
            mode: mode.min(2),
            _pad: [0.0; 2],
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Mirror Pass",
            ctx.width, ctx.height,
            profiler,
        );
    }
}
