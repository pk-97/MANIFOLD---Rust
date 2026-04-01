use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use super::compute_blit_helper::ComputeBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdgeDetectUniforms {
    amount: f32,
    threshold: f32,
    mode: u32,
    texel_size_x: f32,
    texel_size_y: f32,
    _pad: [f32; 3],
}

/// Edge Detect WGSL source — shared across all specialized mode variants.
const EDGE_DETECT_WGSL: &str = include_str!("shaders/fx_edge_detect.wgsl");

/// Edge detection effect — Sobel, Laplacian, or Frei-Chen edge detection.
/// Pure edge detect without glow. Use Bloom or Halation after for glow.
/// Stateless single-pass compute shader.
pub struct EdgeDetectFX {
    helper: ComputeBlitHelper,
    /// Specialized compute pipelines per edge detection mode: Sobel=0, Laplacian=1, Frei-Chen=2.
    pipeline_sobel: manifold_gpu::GpuComputePipeline,
    pipeline_laplacian: manifold_gpu::GpuComputePipeline,
    pipeline_frei_chen: manifold_gpu::GpuComputePipeline,
}

impl EdgeDetectFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let spec = |mode: &str, label: &str| {
            device.create_specialized_compute_pipeline(
                EDGE_DETECT_WGSL,
                "cs_main",
                &[("uniforms.mode", mode)],
                label,
            )
        };
        Self {
            helper: ComputeBlitHelper::new(device, EDGE_DETECT_WGSL, "EdgeDetect"),
            pipeline_sobel: spec("0u", "EdgeDetect Sobel"),
            pipeline_laplacian: spec("1u", "EdgeDetect Laplacian"),
            pipeline_frei_chen: spec("2u", "EdgeDetect Frei-Chen"),
        }
    }
}

impl PostProcessEffect for EdgeDetectFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::EDGE_DETECT
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
        // Legacy project files have 4 params (p[2] was glow, now removed).
        // New projects have 3 params. Read mode from the correct index.
        let mode_raw = if p.len() >= 4 {
            p[3] // legacy layout: skip removed p[2]
        } else {
            p.get(2).copied().unwrap_or(0.0)
        };
        let uniforms = EdgeDetectUniforms {
            amount:       p.first().copied().unwrap_or(0.0),
            threshold:    p.get(1).copied().unwrap_or(0.1),
            mode:         mode_raw.round() as u32,
            texel_size_x: 1.0 / ctx.output_width as f32,
            texel_size_y: 1.0 / ctx.output_height as f32,
            _pad: [0.0; 3],
        };

        let pipeline = match uniforms.mode {
            1 => &self.pipeline_laplacian,
            2 => &self.pipeline_frei_chen,
            _ => &self.pipeline_sobel,
        };
        self.helper.dispatch_with(
            pipeline, gpu,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "EdgeDetect Pass",
            ctx.width, ctx.height,
        );
    }
}
