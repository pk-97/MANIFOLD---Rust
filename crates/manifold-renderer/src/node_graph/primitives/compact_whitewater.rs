//! `node.compact_whitewater` — the kept whitewater slots moved to the pool's
//! front (`docs/GPU_WHITEWATER_DESIGN.md` section 3.9, phase L4). A
//! per-element gather on the codegen path over `node.keep_whitewater`'s
//! flags and their `node.running_total`.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::WhitewaterParticle;

/// Codegen uniform layout: `dispatch_count`, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct CountUniforms {
    pub dispatch_count: u32,
    pub _pad: [u32; 3],
}

crate::primitive! {
    name: CompactWhitewater,
    type_id: "node.compact_whitewater",
    purpose: "Drops the removed whitewater slots: the kept ones move to the pool's front in pool order, the rest become empty (kind 3), and the pool's header, its last slot, passes whole. The kept slot for position j is the first whose running total of keep flags reaches j + 1.",
    inputs: {
        pool: Array(WhitewaterParticle) required,
        kept: Array(WhitewaterParticle) required,
        scan: Array(u32) required,
    },
    outputs: {
        out: Array(WhitewaterParticle),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Ends the GPU whitewater tick, its output fed back as next frame's pool. Wire pool and kept from the same pool, scan from a node.running_total over node.keep_whitewater's flags for that pool.",
    examples: [],
    picker: { label: "Compact Whitewater", category: Atom },
    summary: "Closes the gaps the removed foam, spray and bubbles leave, so the survivors sit together at the front.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater compaction", "remove particles", "stream compaction"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/compact_whitewater_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "pool" },
    wgsl_includes: [],
}

impl Primitive for CompactWhitewater {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "pool").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(pool), Some(kept), Some(scan), Some(out)) =
            (ctx.inputs.array("pool"), ctx.inputs.array("kept"), ctx.inputs.array("scan"), ctx.outputs.array("out"))
        else {
            return;
        };
        let count = (pool.size.min(out.size) / std::mem::size_of::<WhitewaterParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let uniforms = CountUniforms { dispatch_count: count, _pad: [0; 3] };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: pool, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: kept, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: scan, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.compact_whitewater",
        );
    }
}
