//! `node.collar_gather` — lattice values at the collar entries
//! (docs/FFT_WATER_SOLVER_DESIGN.md D3, D10): the second half of the collar
//! operator. Entry e reads its cell and subtracts the vector's constant c;
//! the element after the entries is sum/volume, the constraint row that ties
//! the collar sources to the divergence total. A per-element gather on the
//! codegen path.

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: no params, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GatherUniforms {
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: CollarGather,
    type_id: "node.collar_gather",
    purpose: "A collar vector from a lattice: for the K entries (cell indices from node.select_flagged), out[e] = grid[entries[e]] − c, empty entries 0; out[K] = sum[0] / (lattice cells). c is vector[K], the constant after the entries of the collar vector the grid was built from; a vector with no element K subtracts nothing. The output has K + 1 elements.",
    inputs: {
        entries: Array(u32) required,
        grid: Array(f32) required,
        vector: Array(f32) required,
        sum: Array(f32) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Inside the Krylov loop: grid is the box solve of node.collar_source(z), vector is z, sum is node.dot_products of z's entries (no vector wired, row length K). For the right-hand side: grid is the box solve of the divergence, sum is the divergence total, and vector is that one-element sum, so nothing is subtracted.",
    examples: [],
    picker: { label: "Collar Gather", category: Atom },
    summary: "Reads the grid back at the water's edge for the pressure solver.",
    category: Particles3D,
    role: Filter,
    aliases: ["gather from grid", "lattice to collar"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/collar_gather_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather],
}

impl Primitive for CollarGather {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out")
            .then(|| inputs.iter().find(|(name, _)| *name == "entries").map(|&(_, n)| n + 1))
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(entries), Some(grid), Some(vector), Some(sum), Some(out)) = (
            ctx.inputs.array("entries"),
            ctx.inputs.array("grid"),
            ctx.inputs.array("vector"),
            ctx.inputs.array("sum"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let count = ((entries.size / 4 + 1).min(out.size / 4)) as u32;
        if grid.size < 4 || vector.size < 4 || sum.size < 4 {
            ctx.error("Collar Gather: empty arrays".to_string());
            return;
        }
        let uniforms = GatherUniforms { dispatch_count: count, _pad0: 0, _pad1: 0, _pad2: 0 };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: entries, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: grid, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: vector, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: sum, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.collar_gather",
        );
    }
}
