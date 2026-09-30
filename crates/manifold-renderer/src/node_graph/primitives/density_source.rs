//! `node.density_source` — the FFT water step's answer to particle clumping
//! (docs/FFT_WATER_SOLVER_DESIGN.md P3): a cell holding more particles than
//! the fill put there asks a solve to spread it, and inside the water a cell
//! holding fewer asks it to close. A correction that only spreads ratchets
//! the water outward, because packing noise runs both ways. The step solves
//! it on its own and moves particles by the result without keeping it as
//! velocity: kept, a fast splash's correction becomes speed. A per-element
//! atom on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::cell_capacity;
use super::collar_cells::{cell_count, cell_lattice};
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::CellRange;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DensityUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    rest: f32,
    rate: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: DensitySource,
    type_id: "node.density_source",
    purpose: "The crowding target of a density solve that evens out particle packing: with count[c] the particles a sort put in cell c (cell_ranges, lattice order) and e = count[c] / rest − 1, out[c] = −rate · e inside the water (all six neighbours hold particles or lie past the lattice) and −rate · max(e, 0) at its surface; empty cells are 0. Solved as a pressure right-hand side, the field it gives expands crowded cells and, inside the water, closes sparse ones at that rate (1/s).",
    inputs: {
        cell_ranges: Array(CellRange) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
        float_param!("rest", "Rest Particles Per Cell", 8.0, 1.0, 64.0),
        float_param!("rate", "Spread Rate (1/s)", 1.0, 0.0, 1000.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The right-hand side of the FFT water step's density solve, from the cell_ranges of the sort that binned the step's particles by the lattice's cells. The solve's pressure goes through node.subtract_pressure onto the projected faces, and those faces reach node.faces_to_particles as `advect` only, so particles move apart without gaining speed. rest is the fill's particles per cell (8).",
    examples: [],
    picker: { label: "Density Source", category: Atom },
    summary: "Pushes apart liquid particles that have bunched up, so the water keeps its volume.",
    category: Particles3D,
    role: Filter,
    aliases: ["density correction", "anti clumping", "volume correction", "particle spacing"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/density_source_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for DensitySource {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| cell_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Density Source: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let rest = ctx.scalar_or_param("rest", 8.0);
        let rate = ctx.scalar_or_param("rate", 1.0);
        if !(rest.is_finite() && rest >= 1.0 && rate.is_finite() && rate >= 0.0) {
            ctx.error("Density Source: rest must be at least 1 and rate at least 0".to_string());
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(ranges), Some(out)) = (ctx.inputs.array("cell_ranges"), ctx.outputs.array("out")) else {
            return;
        };
        let cells = cell_count(nodes);
        if cells * 4 > out.size || cells * std::mem::size_of::<CellRange>() as u64 > ranges.size {
            ctx.error(format!("Density Source: a {nodes:?} lattice is larger than its arrays; bin the sort by the lattice's cells"));
            return;
        }
        let uniforms = DensityUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            rest,
            rate,
            dispatch_count: cells as u32,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: ranges, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [(cells as u32).div_ceil(256), 1, 1],
            "node.density_source",
        );
    }
}
