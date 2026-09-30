//! `node.cells_with_particles` — the water lattice of a particle liquid
//! (docs/FFT_WATER_SOLVER_DESIGN.md section 3 step 2): a cell is water when
//! the sort put a particle in it. A per-element atom on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::freeze::classify::FusedOutputCapacity;
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::CellRange;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// The lattice params whose product is a lattice atom's cell count.
pub(super) const LATTICE_PARAMS: [&str; 3] = ["nodes_x", "nodes_y", "nodes_z"];

/// Lattice lengths of the liquid's cell atoms, 1 to 1024 per axis, or `None`.
pub(crate) fn cell_lattice(params: &ParamValues) -> Option<[u32; 3]> {
    let nodes = LATTICE_PARAMS.map(|name| match params.get(name) {
        Some(ParamValue::Float(n)) => n.round() as i64,
        _ => 64,
    });
    nodes.iter().all(|n| (1..=1024).contains(n)).then(|| nodes.map(|n| n as u32))
}

/// Cells in a lattice, as u64 so a bad size cannot wrap.
pub(crate) fn cell_count(nodes: [u32; 3]) -> u64 {
    nodes.iter().map(|&n| u64::from(n)).product()
}

/// Lattice cells for a param set, for `array_output_capacity`: the sort
/// sizes its ranges only at run time, so the lattice sizes this output.
pub(super) fn cell_capacity(params: &ParamValues) -> Option<u32> {
    cell_lattice(params).and_then(|nodes| u32::try_from(cell_count(nodes)).ok())
}

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CellsUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: CellsWithParticles,
    type_id: "node.cells_with_particles",
    purpose: "Mark which cells of a lattice hold particles, from a sort whose bins are the lattice's cells: out[c] = 1 where cell_ranges[c].count > 0, else 0, for the nodes_x × nodes_y × nodes_z cells in the sort's bin order.",
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
    ],
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
    output_capacity: FusedOutputCapacity::ParamProduct { params: &LATTICE_PARAMS },
}

impl Primitive for CellsWithParticles {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| cell_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Cells With Particles: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(ranges), Some(out)) = (ctx.inputs.array("cell_ranges"), ctx.outputs.array("out")) else {
            return;
        };
        let cells = cell_count(nodes);
        if cells * std::mem::size_of::<CellRange>() as u64 > ranges.size || cells * 4 > out.size {
            ctx.error(format!("Cells With Particles: a {nodes:?} lattice is larger than its arrays; bin the sort by the lattice's cells"));
            return;
        }
        let count = cells as u32;
        let uniforms = CellsUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            dispatch_count: count,
        };
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
