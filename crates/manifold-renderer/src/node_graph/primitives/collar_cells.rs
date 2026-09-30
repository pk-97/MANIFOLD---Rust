//! `node.collar_cells` — the one-cell air collar around the water
//! (docs/FFT_WATER_SOLVER_DESIGN.md D3): the air cells with a water face
//! neighbour, where the pressure solve's unknown sources live. A per-element
//! gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Lattice lengths of the SWASH cell atoms, 1 to 1024 per axis, or `None`.
pub(super) fn cell_lattice(params: &ParamValues) -> Option<[u32; 3]> {
    let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| match params.get(name) {
        Some(ParamValue::Float(n)) => n.round() as i64,
        _ => 64,
    });
    nodes.iter().all(|n| (1..=1024).contains(n)).then(|| nodes.map(|n| n as u32))
}

/// Cells in a lattice, as u64 so a bad size cannot wrap.
pub(super) fn cell_count(nodes: [u32; 3]) -> u64 {
    nodes.iter().map(|&n| u64::from(n)).product()
}

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CollarUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: CollarCells,
    type_id: "node.collar_cells",
    purpose: "Flag the air collar of a water lattice: out[c] = 1 where cell c is air (water[c] ≤ 0.5) and at least one of its six face neighbours inside the lattice is water, else 0. Lattice nodes_x/y/z, cell (i, j, k) at i + nx·(j + ny·k); the box walls are not water.",
    inputs: {
        water: Array(f32) required,
    },
    outputs: {
        out: Array(u32),
    },
    params: [
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The first step of the FFT water pressure solve: collar_cells → node.running_total → node.select_flagged lists the collar cells in cell order. water is 1 in water cells and 0 in air.",
    examples: [],
    picker: { label: "Collar Cells", category: Atom },
    summary: "Marks the layer of air cells touching the water, where the pressure solver works.",
    category: Particles3D,
    role: Filter,
    aliases: ["surface cells", "air collar", "free surface", "interface cells"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/collar_cells_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for CollarCells {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "water").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Collar Cells: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(water), Some(out)) = (ctx.inputs.array("water"), ctx.outputs.array("out")) else {
            return;
        };
        let cells = cell_count(nodes);
        if cells * 4 > water.size.min(out.size) {
            ctx.error(format!("Collar Cells: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = CollarUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            dispatch_count: cells as u32,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: water, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [(cells as u32).div_ceil(256), 1, 1],
            "node.collar_cells",
        );
    }
}
