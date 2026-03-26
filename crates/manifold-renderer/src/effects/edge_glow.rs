use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use super::compute_blit_helper::ComputeBlitHelper;

// EdgeGlowFX.cs:16-19 — shader property IDs (_Amount, _Threshold, _Glow, _Mode)
// + _MainTex_TexelSize from the shader (xy = 1/width, 1/height)
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdgeGlowUniforms {
    amount: f32,        // EdgeGlowFX.cs:23 — _Amount   = GetParam(0)
    threshold: f32,     // EdgeGlowFX.cs:24 — _Threshold = GetParam(1)
    glow: f32,          // EdgeGlowFX.cs:25 — _Glow     = GetParam(2)
    mode: u32,          // EdgeGlowFX.cs:26 — _Mode     = Mathf.Round(GetParam(3)) as u32
    texel_size_x: f32,  // EdgeGlowEffect.shader:133 — _MainTex_TexelSize.x = 1.0 / source_width
    texel_size_y: f32,  // EdgeGlowEffect.shader:133 — _MainTex_TexelSize.y = 1.0 / source_height
    _pad: [f32; 2],
}

/// Edge detection with soft glow.
/// Stateless single-pass effect. Translated from EdgeGlowFX.cs + EdgeGlowEffect.shader.
pub struct EdgeGlowFX {
    helper: ComputeBlitHelper,
}

impl EdgeGlowFX {
    pub fn new(
        device: &wgpu::Device,
        hal_ctx: Option<&crate::hal_context::HalContext>,
        #[cfg(target_os = "macos")] native_device: Option<&manifold_gpu::GpuDevice>,
    ) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_edge_glow_compute.wgsl"),
                "EdgeGlow",
                std::mem::size_of::<EdgeGlowUniforms>() as u64,
                hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            ),
        }
    }
}

impl PostProcessEffect for EdgeGlowFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::EDGE_GLOW
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
        // EdgeGlowFX.cs:22-27 — SetUniforms: GetParam(0..3), Mathf.Round for mode
        // EdgeGlowEffect.shader:133 — texel size comes from source texture dimensions
        let p = &fx.param_values;
        let uniforms = EdgeGlowUniforms {
            amount:       p.first().copied().unwrap_or(0.0),           // p[0] default 0
            threshold:    p.get(1).copied().unwrap_or(0.3),            // p[1] default 0.3
            glow:         p.get(2).copied().unwrap_or(0.5),            // p[2] default 0.5
            mode:         p.get(3).copied().unwrap_or(0.0).round() as u32, // p[3] discrete, default 0
            texel_size_x: 1.0 / ctx.width as f32,
            texel_size_y: 1.0 / ctx.height as f32,
            _pad: [0.0; 2],
        };

        self.helper.dispatch(
            gpu,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "EdgeGlow Pass",
            ctx.width, ctx.height,
            profiler,
        );
    }
}
