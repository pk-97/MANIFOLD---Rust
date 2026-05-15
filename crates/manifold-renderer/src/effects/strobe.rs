use std::borrow::Cow;

use super::compute_blit_helper::ComputeBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::{STROBE_NOTE_RATES, Strobe};
use crate::node_graph::{
    ChainSpec, Graph, NodeInstanceId, ParamConvert, Routing, SkipMode, SpliceResult,
};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::STROBE,
        display_name: "Strobe",
        category: "Post-Process",
        available: true,
        osc_prefix: "strobe",
        legacy_discriminant: Some(19),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
            ParamSpec::whole_labels("rate", "Rate", 0.0, 9.0, 6.0, &["1/1", "1/2", "1/4", "1/4T", "1/8", "1/8T", "1/16", "1/16T", "1/32", "1/64"], "Rate"),
            ParamSpec::whole_labels("mode", "Mode", 0.0, 2.0, 0.0, &["Opacity", "White", "Gain"], "Mode"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::STROBE,
        create: |device| Box::new(StrobeFX::new(device)),
    }
}

fn splice_strobe(graph: &mut Graph, source: (NodeInstanceId, &'static str)) -> SpliceResult {
    let node = graph.add_node(Box::new(Strobe::new()));
    graph.connect(source, (node, "in")).expect("wire source → Strobe.in");
    SpliceResult {
        output: (node, "out"),
        handles: vec![(Cow::Borrowed("strobe"), node)],
    }
}

/// Legacy `rate` is an index into the note-rate table (0..9);
/// `Strobe` takes the raw strobes-per-beat float. Mirrors what
/// `StrobeFX::apply` did inline before encoding its uniform.
fn strobe_rate_from_index(idx_f: f32) -> f32 {
    let idx = idx_f.max(0.0).round() as usize;
    STROBE_NOTE_RATES
        .get(idx)
        .copied()
        .unwrap_or(*STROBE_NOTE_RATES.last().unwrap_or(&1.0))
}

inventory::submit! {
    ChainSpec {
        type_id: EffectTypeId::STROBE,
        splice: splice_strobe,
        routings: &[
            Routing { param_id: "amount", target_handle: "strobe", target_param: "amount", convert: ParamConvert::Float },
            Routing { param_id: "rate", target_handle: "strobe", target_param: "rate", convert: ParamConvert::FloatTransform(strobe_rate_from_index) },
            Routing { param_id: "mode", target_handle: "strobe", target_param: "mode", convert: ParamConvert::EnumRound },
            // `beat` is ctx-driven — populated each frame by
            // `apply_ctx_params_at` from `EffectContext::beat`.
        ],
        skip: SkipMode::OnZero { param_id: "amount" },
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StrobeUniforms {
    amount: f32,
    rate: f32,
    mode: u32, // 0=Opacity(black), 1=White, 2=Gain
    beat: f32,
}

/// NoteRates lookup table mapping param index to strobes-per-beat.
/// Unity ref: StrobeFX.cs lines 12-27
const NOTE_RATES: [f32; 10] = [0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 16.0];

/// Strobe effect — beat-synced square wave flash.
/// Uses fragment shader for TBDR tile memory on Apple Silicon.
pub struct StrobeFX {
    helper: ComputeBlitHelper,
}

impl StrobeFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        Self {
            helper: ComputeBlitHelper::new(
                device,
                include_str!("shaders/fx_strobe.wgsl"),
                "Strobe",
            ),
        }
    }
}

impl PostProcessEffect for StrobeFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::STROBE
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
        // Map rate index through NoteRates lookup table (Unity: StrobeFX.cs lines 24-26)
        let rate_idx = p.get(1).map(|pv| pv.value).unwrap_or(6.0).round().max(0.0) as usize;
        let rate = NOTE_RATES[rate_idx.min(NOTE_RATES.len() - 1)];
        let uniforms = StrobeUniforms {
            amount: p.first().map(|pv| pv.value).unwrap_or(0.0),
            rate,
            mode: (p.get(2).map(|pv| pv.value).unwrap_or(0.0).round() as u32).min(2),
            beat: ctx.beat,
        };

        self.helper.dispatch(
            gpu,
            source,
            target,
            bytemuck::bytes_of(&uniforms),
            "Strobe Pass",
            ctx.width,
            ctx.height,
        );
    }
}
