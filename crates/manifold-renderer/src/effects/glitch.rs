use manifold_core::EffectType;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlitchUniforms {
    amount: f32,
    block_size: f32,
    rgb_shift: f32,
    scanline: f32,   // GlitchFX.cs:16 — _Scanline
    speed: f32,
    time: f32,
    resolution_x: f32,
    resolution_y: f32,
}

/// Glitch effect — block displacement, scanline jitter, RGB channel split.
pub struct GlitchFX {
    helper: SimpleBlitHelper,
}

impl GlitchFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/fx_glitch.wgsl"),
                "Glitch",
                std::mem::size_of::<GlitchUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for GlitchFX {
    fn effect_type(&self) -> EffectType {
        EffectType::Glitch
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
        // GlitchFX.cs:13-18 — read all 5 params in Unity order
        let p = &fx.param_values;
        let uniforms = GlitchUniforms {
            amount: p.first().copied().unwrap_or(0.0),                // line 13: _Amount
            block_size: p.get(1).copied().unwrap_or(16.0).max(4.0),   // line 14: _BlockSize
            rgb_shift: p.get(2).copied().unwrap_or(0.01),             // line 15: _RGBShift
            scanline: p.get(3).copied().unwrap_or(0.3),               // line 16: _Scanline
            speed: p.get(4).copied().unwrap_or(2.0),                  // line 17: _Speed
            time: ctx.time,                                            // line 18: Time.time
            resolution_x: ctx.width as f32,
            resolution_y: ctx.height as f32,
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "Glitch Pass",
            profiler,
        );
    }
}
