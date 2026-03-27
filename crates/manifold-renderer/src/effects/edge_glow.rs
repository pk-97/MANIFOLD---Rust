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

/// Edge glow WGSL source — shared across all specialized mode variants.
const EDGE_GLOW_WGSL: &str = include_str!("shaders/fx_edge_glow_compute.wgsl");

/// Edge detection with soft glow.
/// Stateless single-pass effect. Translated from EdgeGlowFX.cs + EdgeGlowEffect.shader.
pub struct EdgeGlowFX {
    helper: ComputeBlitHelper,
    /// Specialized pipelines per edge detection mode: Sobel=0, Laplacian=1, Frei-Chen=2.
    /// Metal compiler eliminates inactive if/else branches in detect_edge().
    pipeline_sobel: manifold_gpu::GpuComputePipeline,
    pipeline_laplacian: manifold_gpu::GpuComputePipeline,
    pipeline_frei_chen: manifold_gpu::GpuComputePipeline,
}

impl EdgeGlowFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let spec = |mode: &str, label: &str| {
            device.create_specialized_compute_pipeline(
                EDGE_GLOW_WGSL,
                "cs_main",
                &[("uniforms.mode", mode)],
                label,
            )
        };
        Self {
            helper: ComputeBlitHelper::new(device, EDGE_GLOW_WGSL, "EdgeGlow"),
            pipeline_sobel: spec("0u", "EdgeGlow Sobel"),
            pipeline_laplacian: spec("1u", "EdgeGlow Laplacian"),
            pipeline_frei_chen: spec("2u", "EdgeGlow Frei-Chen"),
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
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
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

        // Select specialized pipeline based on edge detection mode
        let pipeline = match uniforms.mode {
            1 => &self.pipeline_laplacian,
            2 => &self.pipeline_frei_chen,
            _ => &self.pipeline_sobel,
        };
        self.helper.dispatch_with(
            pipeline, gpu,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "EdgeGlow Pass",
            ctx.width, ctx.height,
        );
    }
}
