use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::gpu_encoder::GpuEncoder;
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;
use manifold_core::effects::EffectInstance;
use crate::effects::registration::EffectFactory;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::EDGE_STRETCH,
        display_name: "Edge Stretch",
        category: "Post-Process",
        available: true,
        osc_prefix: "edgeStretch",
        legacy_discriminant: Some(15),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),
            ParamSpec::continuous("width", "Width", 0.1, 0.9, 0.433, "F2", "SourceWidth"),
            ParamSpec::whole_labels("dir", "Dir", 0.0, 2.0, 0.0, &["Horiz", "Vert", "Both"], "Direction"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::EDGE_STRETCH,
        create: |device| Box::new(EdgeStretchFX::new(device)),
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdgeStretchUniforms {
    amount: f32,
    source_width: f32,
    mode: u32, // 0=Horizontal, 1=Vertical, 2=Both
    _pad: f32,
}

/// EdgeStretch effect — clamps UVs to a center strip, stretching edge pixels.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct EdgeStretchFX {
    helper: ComputeBlitHelper,
}

impl EdgeStretchFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_edge_stretch.wgsl"),
                "EdgeStretch",
            ),
        }
    }
}

impl PostProcessEffect for EdgeStretchFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::EDGE_STRETCH
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
        let uniforms = EdgeStretchUniforms {
            // EdgeStretchFX.cs:13-15 — GetParam(0), GetParam(1), Mathf.Round(GetParam(2))
            amount: p.first().copied().unwrap_or(1.0),
            source_width: p.get(1).copied().unwrap_or(0.433).clamp(0.1, 0.9),
            mode: (p.get(2).copied().unwrap_or(0.0).round() as u32).min(2),
            _pad: 0.0,
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "EdgeStretch Pass",
            ctx.width,
            ctx.height,
        );
    }
}
