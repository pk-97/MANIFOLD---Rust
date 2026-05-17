use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::EdgeDetect;
use crate::node_graph::{ParamBinding, ParamConvert, ParamTarget, SkipMode};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::{EffectAliasMetadata, EffectMetadata};
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;
use std::borrow::Cow;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::EDGE_DETECT,
        display_name: "Edge Detect",
        category: "Diagnostic",
        available: true,
        osc_prefix: "edgeDetect",
        legacy_discriminant: Some(25),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),
            ParamSpec::continuous("threshold", "Threshold", 0.0, 1.0, 0.1, "F2", "Threshold"),
            ParamSpec::whole_labels("mode", "Mode", 0.0, 2.0, 0.0, &["Sobel", "Laplacian", "Frei-Chen"], "Mode"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::EDGE_DETECT,
        create: |device| Box::new(EdgeDetectFX::new(device)),
    }
}

inventory::submit! {
    EffectAliasMetadata {
        id: EffectTypeId::EDGE_DETECT,
        aliases: &[("thresh", Some("threshold"))],
    }
}

crate::atomic_chain_spec! {
    type_id: EffectTypeId::EDGE_DETECT,
    primitive: EdgeDetect,
    handle: "edge_detect",
    bindings: &[
        ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,
            target: ParamTarget::HandleNode { handle: "edge_detect", param: "amount" },
            convert: ParamConvert::Float,
        },
        ParamBinding {
            id: Cow::Borrowed("threshold"),
            label: "Threshold",
            default_value: 0.1,
            target: ParamTarget::HandleNode { handle: "edge_detect", param: "threshold" },
            convert: ParamConvert::Float,
        },
        // Legacy `mode` (Sobel/Laplacian/Frei-Chen) was a binary
        // toggle the primitive folded into the always-on shader
        // path. Intentionally unrouted — preserves the existing
        // EdgeDetect parity-tested behavior. The metadata's `mode`
        // ParamSpec is dropped; users see only Amount + Thresh.
    ],
    skip: SkipMode::OnZero { param_id: "amount" },
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdgeDetectUniforms {
    amount: f32,
    threshold: f32,
    texel_size_x: f32,
    texel_size_y: f32,
}

/// Edge detection effect — Sobel 3x3 edge detection.
/// Pure edge detect without glow. Use Bloom or Halation after for glow.
pub struct EdgeDetectFX {
    helper: ComputeBlitHelper,
}

impl EdgeDetectFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_edge_detect.wgsl"),
                "EdgeDetect",
            ),
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
        let uniforms = EdgeDetectUniforms {
            amount: p.first().map(|pv| pv.value).unwrap_or(0.0),
            threshold: p.get(1).map(|pv| pv.value).unwrap_or(0.1),
            texel_size_x: 1.0 / ctx.output_width as f32,
            texel_size_y: 1.0 / ctx.output_height as f32,
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "EdgeDetect Pass",
            ctx.width,
            ctx.height,
        );
    }
}
