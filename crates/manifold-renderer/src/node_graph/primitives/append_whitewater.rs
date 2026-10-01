//! `node.append_whitewater` — the frame's live spawns into a compacted
//! whitewater pool's empty slots (`docs/GPU_WHITEWATER_DESIGN.md` section
//! 3.9, phase L4). A per-element gather on the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use manifold_fluids::WhitewaterSpawn;
use manifold_gpu::GpuBinding;

use super::compact_whitewater::CountUniforms;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::WhitewaterParticle;

crate::primitive! {
    name: AppendWhitewater,
    type_id: "node.append_whitewater",
    purpose: "Adds the frame's live whitewater spawns, in spawn order, to a compacted pool: they fill the empty slots after its particles, each taking the next id (mod 256) from the pool's header, its last slot. Spawns past the pool's room are dropped, and the header records how many in pad0 (0 when all fit). Every other slot passes whole.",
    inputs: {
        pool: Array(WhitewaterParticle) required,
        compacted: Array(WhitewaterParticle) required,
        spawns: Array(WhitewaterSpawn) required,
        live_scan: Array(u32) required,
    },
    outputs: {
        out: Array(WhitewaterParticle),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Starts the GPU whitewater tick, ahead of node.advect_whitewater, as the lifecycle loads spawns before it steps. Wire pool and compacted from the same pool, last frame's node.compact_whitewater output through node.array_feedback; spawns from node.whitewater_type, live_scan a node.running_total over node.live_whitewater_spawns on those spawns.",
    examples: [],
    picker: { label: "Append Whitewater", category: Atom },
    summary: "Drops this frame's new foam, spray and bubbles into the free slots of the whitewater pool.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater load", "add particles", "append spawns"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/append_whitewater_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "pool" },
    wgsl_includes: [],
}

impl Primitive for AppendWhitewater {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "pool").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(pool), Some(compacted), Some(spawns), Some(live_scan), Some(out)) = (
            ctx.inputs.array("pool"),
            ctx.inputs.array("compacted"),
            ctx.inputs.array("spawns"),
            ctx.inputs.array("live_scan"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let record = std::mem::size_of::<WhitewaterParticle>() as u64;
        let count = (pool.size.min(out.size) / record) as u32;
        if count == 0 {
            return;
        }
        if live_scan.size / 4 != spawns.size / std::mem::size_of::<WhitewaterSpawn>() as u64 {
            ctx.error(format!(
                "Append Whitewater: live_scan holds {} counts for {} spawn slots; wire it from a node.running_total over these spawns' node.live_whitewater_spawns",
                live_scan.size / 4,
                spawns.size / std::mem::size_of::<WhitewaterSpawn>() as u64
            ));
            return;
        }
        let uniforms = CountUniforms { dispatch_count: count, _pad: [0; 3] };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: pool, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: compacted, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: spawns, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: live_scan, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.append_whitewater",
        );
    }
}
