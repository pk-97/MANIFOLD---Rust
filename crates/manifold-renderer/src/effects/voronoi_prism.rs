use std::borrow::Cow;

use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::VoronoiPrism;
use crate::node_graph::{
    ChainSpec, Graph, NodeInstanceId, ParamConvert, Routing, SkipMode, SpliceResult,
};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::VORONOI_PRISM,
        display_name: "Voronoi Prism",
        category: "Post-Process",
        available: true,
        osc_prefix: "voronoiPrism",
        legacy_discriminant: Some(16),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
            ParamSpec::whole("cells", "Cells", 4.0, 64.0, 16.0, "CellCount"),
            ParamSpec::continuous("source_width", "Cell Size", 0.1, 0.9, 0.5625, "F2", "SourceWidth"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::VORONOI_PRISM,
        create: |device| Box::new(VoronoiPrismFX::new(device)),
    }
}

fn splice_voronoi_prism(graph: &mut Graph, source: (NodeInstanceId, &'static str)) -> SpliceResult {
    let node = graph.add_node(Box::new(VoronoiPrism::new()));
    graph.connect(source, (node, "in")).expect("wire source → VoronoiPrism.in");
    SpliceResult {
        output: (node, "out"),
        handles: vec![(Cow::Borrowed("voronoi"), node)],
    }
}

inventory::submit! {
    ChainSpec {
        type_id: EffectTypeId::VORONOI_PRISM,
        splice: splice_voronoi_prism,
        routings: &[
            Routing { param_id: "amount", target_handle: "voronoi", target_param: "amount", convert: ParamConvert::Float },
            Routing { param_id: "cells", target_handle: "voronoi", target_param: "cell_count", convert: ParamConvert::Float },
            // `source_width` was previously populated by a hidden
            // cross-effect read from EdgeStretch's `width` slider via
            // `EffectContext::edge_stretch_width`. Now it's an explicit
            // user slider on the VoronoiPrism card — same default
            // (0.5625), no invisible coupling. Existing projects that
            // omit this slot fall back to the metadata default.
            Routing { param_id: "source_width", target_handle: "voronoi", target_param: "source_width", convert: ParamConvert::Float },
            // `beat` stays ctx-driven (populated by `apply_ctx_params_at`
            // from `EffectContext::beat`).
        ],
        skip: SkipMode::OnZero { param_id: "amount" },
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VoronoiPrismUniforms {
    amount: f32,
    cell_count: f32,
    beat: f32,
    aspect_ratio: f32,
    source_width: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// VoronoiPrism effect — per-cell UV remapping with beat-synchronized pop-in.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
/// Unity ref: VoronoiPrismFX.cs / VoronoiPrismEffect.shader
pub struct VoronoiPrismFX {
    helper: ComputeBlitHelper,
}

impl VoronoiPrismFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_voronoi_prism.wgsl"),
                "VoronoiPrism",
            ),
        }
    }
}

impl PostProcessEffect for VoronoiPrismFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::VORONOI_PRISM
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
        let uniforms = VoronoiPrismUniforms {
            amount: p.first().map(|pv| pv.value).unwrap_or(0.0),
            cell_count: p.get(1).map(|pv| pv.value).unwrap_or(16.0),
            beat: ctx.beat,
            aspect_ratio: ctx.width as f32 / ctx.height as f32,
            source_width: p.get(2).map(|pv| pv.value).unwrap_or(0.5625),
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "VoronoiPrism Pass",
            ctx.width,
            ctx.height,
        );
    }
}
