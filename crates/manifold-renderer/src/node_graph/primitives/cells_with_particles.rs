//! `node.cells_with_particles` — the water lattice of a particle liquid
//! (docs/FFT_WATER_SOLVER_DESIGN.md section 3 step 2): a cell is water when
//! the sort put a particle in it. A per-element atom on the codegen path.

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::CellRange;
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: no params, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CellsUniforms {
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: CellsWithParticles,
    type_id: "node.cells_with_particles",
    purpose: "Mark which bins of a particle sort hold particles: out[c] = 1 where cell_ranges[c].count > 0, else 0. One element per bin, in the sort's bin order.",
    inputs: {
        cell_ranges: Array(CellRange) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "After node.sort_particles_into_cells whose bins are the liquid's lattice cells (the box is the lattice, cell_size its cell): the 1/0 water lattice the FFT water pressure solve and node.face_divergence read.",
    examples: [],
    picker: { label: "Cells With Particles", category: Atom },
    summary: "Marks the grid cells that have liquid in them.",
    category: Particles3D,
    role: Filter,
    aliases: ["water cells", "occupied cells", "fluid mask"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/cells_with_particles_body.wgsl"),
    input_access: [Coincident],
}

impl Primitive for CellsWithParticles {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "cell_ranges").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(ranges), Some(out)) = (ctx.inputs.array("cell_ranges"), ctx.outputs.array("out")) else {
            return;
        };
        let count = (ranges.size / std::mem::size_of::<CellRange>() as u64).min(out.size / 4) as u32;
        if count == 0 {
            return;
        }
        let uniforms = CellsUniforms { dispatch_count: count, _pad0: 0, _pad1: 0, _pad2: 0 };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: ranges, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.cells_with_particles",
        );
    }
}
