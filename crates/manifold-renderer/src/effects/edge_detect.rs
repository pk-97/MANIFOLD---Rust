use std::borrow::Cow;

use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::EdgeDetect;
use crate::node_graph::{
    ChainSpec, Graph, NodeInstanceId, ParamConvert, Routing, SkipMode, SpliceResult,
};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::EDGE_DETECT,
        display_name: "Edge Detect",
        category: "Post-Process",
        available: true,
        osc_prefix: "edgeDetect",
        legacy_discriminant: Some(25),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
            ParamSpec::continuous("thresh", "Thresh", 0.0, 1.0, 0.1, "F2", "Threshold"),
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

fn splice_edge_detect(graph: &mut Graph, source: (NodeInstanceId, &'static str)) -> SpliceResult {
    let node = graph.add_node(Box::new(EdgeDetect::new()));
    graph.connect(source, (node, "in")).expect("wire source → EdgeDetect.in");
    SpliceResult {
        output: (node, "out"),
        handles: vec![(Cow::Borrowed("edge_detect"), node)],
    }
}

inventory::submit! {
    ChainSpec {
        type_id: EffectTypeId::EDGE_DETECT,
        splice: splice_edge_detect,
        routings: &[
            Routing { param_id: "amount", target_handle: "edge_detect", target_param: "amount", convert: ParamConvert::Float },
            Routing { param_id: "thresh", target_handle: "edge_detect", target_param: "threshold", convert: ParamConvert::Float },
            // Legacy `mode` (Sobel/Laplacian/Frei-Chen) was a binary
            // toggle the primitive folded into the always-on shader
            // path. Intentionally unrouted — preserves the existing
            // EdgeDetect parity-tested behavior.
        ],
        skip: SkipMode::OnZero { param_id: "amount" },
    }
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
