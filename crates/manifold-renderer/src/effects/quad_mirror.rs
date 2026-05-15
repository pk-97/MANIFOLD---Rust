use std::borrow::Cow;

use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::QuadMirror;
use crate::node_graph::{
    ChainSpec, Graph, NodeInstanceId, ParamConvert, Routing, SkipMode, SpliceResult,
};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::QUAD_MIRROR,
        display_name: "Quad Mirror",
        category: "Post-Process",
        available: true,
        osc_prefix: "quadMirror",
        legacy_discriminant: Some(17),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::QUAD_MIRROR,
        create: |device| Box::new(QuadMirrorFX::new(device)),
    }
}

fn splice_quad_mirror(graph: &mut Graph, source: (NodeInstanceId, &'static str)) -> SpliceResult {
    let node = graph.add_node(Box::new(QuadMirror::new()));
    graph.connect(source, (node, "in")).expect("wire source → QuadMirror.in");
    SpliceResult {
        output: (node, "out"),
        handles: vec![(Cow::Borrowed("quad_mirror"), node)],
    }
}

inventory::submit! {
    ChainSpec {
        type_id: EffectTypeId::QUAD_MIRROR,
        splice: splice_quad_mirror,
        routings: &[
            Routing { param_id: "amount", target_handle: "quad_mirror", target_param: "amount", convert: ParamConvert::Float },
        ],
        skip: SkipMode::OnZero { param_id: "amount" },
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadMirrorUniforms {
    amount: f32,
    _pad: [f32; 3],
}

/// QuadMirror effect — mirrors UVs around center in both axes with crossfade.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct QuadMirrorFX {
    helper: ComputeBlitHelper,
}

impl QuadMirrorFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_quad_mirror.wgsl"),
                "QuadMirror",
            ),
        }
    }
}

impl PostProcessEffect for QuadMirrorFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::QUAD_MIRROR
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        // QuadMirrorFX.cs:13 — fx.GetParam(0), registry default 1.0
        let amount = fx.param_values.first().map(|p| p.value).unwrap_or(1.0);
        let uniforms = QuadMirrorUniforms {
            amount,
            _pad: [0.0; 3],
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "QuadMirror Pass",
            ctx.width,
            ctx.height,
        );
    }
}
