//! `node.live_whitewater_spawns` — 1 per spawn slot holding a particle
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.9, phase L4), the counts
//! `node.append_whitewater` places by.

use manifold_fluids::WhitewaterSpawn;
use manifold_gpu::GpuBinding;

use super::compact_whitewater::CountUniforms;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: LiveWhitewaterSpawns,
    type_id: "node.live_whitewater_spawns",
    purpose: "1 for each whitewater spawn slot holding a particle (lifetime above 0), 0 for an empty one. One u32 per slot.",
    inputs: {
        spawns: Array(WhitewaterSpawn) required,
    },
    outputs: {
        out: Array(u32),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "On the frame's typed spawns (node.whitewater_type); a node.running_total over the flags is node.append_whitewater's live_scan.",
    examples: [],
    picker: { label: "Live Whitewater Spawns", category: Atom },
    summary: "Marks which of this frame's new foam, spray and bubbles actually exist.",
    category: Particles3D,
    role: Filter,
    aliases: ["spawn flags", "live spawns"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/live_whitewater_spawns_body.wgsl"),
    input_access: [Coincident],
    output_capacity: FusedOutputCapacity::FromInput { input: "spawns" },
    wgsl_includes: [],
}

impl Primitive for LiveWhitewaterSpawns {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "spawns").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(spawns), Some(out)) = (ctx.inputs.array("spawns"), ctx.outputs.array("out")) else { return };
        let count = (spawns.size / std::mem::size_of::<WhitewaterSpawn>() as u64).min(out.size / 4) as u32;
        if count == 0 {
            return;
        }
        let uniforms = CountUniforms { dispatch_count: count, _pad: [0; 3] };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: spawns, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.live_whitewater_spawns",
        );
    }
}
