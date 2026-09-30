//! `node.face_divergence` — the right-hand side of the pressure solve
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md section 1 (the step)): each water cell's net
//! outflow through its faces. A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::cell_capacity;
use super::cells_with_particles::{LATTICE_PARAMS, cell_count, cell_lattice};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use super::particles_to_faces::face_count;
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DivergenceUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: FaceDivergence,
    type_id: "node.face_divergence",
    purpose: "Net outflow of each water cell through its six faces of a face grid (node.particles_to_faces' layout), per second: out[c] = (u(i+1) − u(i) + v(j+1) − v(j) + w(k+1) − w(k)) / cell_size where water[c] > 0.5, else 0.",
    inputs: {
        faces: Array(FaceSample) required,
        water: Array(f32) required,
        cell_size: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.face_gravity; water is node.cells_with_particles. Its output is the f the GPU FLIP pressure solve makes incompressible; node.subtract_pressure then applies the pressure.",
    examples: [],
    picker: { label: "Face Divergence", category: Atom },
    summary: "Measures how much liquid each cell is trying to push out or suck in.",
    category: Particles3D,
    role: Filter,
    aliases: ["divergence", "outflow", "compression"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/face_divergence_body.wgsl"),
    input_access: [BufferGather, Coincident],
    output_capacity: FusedOutputCapacity::ParamProduct { params: &LATTICE_PARAMS },
}

impl Primitive for FaceDivergence {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| cell_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Face Divergence: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        if !(cell_size.is_finite() && cell_size > 0.0) {
            ctx.error("Face Divergence: cell_size must be positive".to_string());
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(faces), Some(water), Some(out)) =
            (ctx.inputs.array("faces"), ctx.inputs.array("water"), ctx.outputs.array("out"))
        else {
            return;
        };
        let cells = cell_count(nodes);
        if cells * 4 > water.size.min(out.size) || face_count(nodes) * 32 > faces.size {
            ctx.error(format!("Face Divergence: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = DivergenceUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            cell_size,
            dispatch_count: cells as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: faces, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: water, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: out, offset: 0 },
            ],
            [(cells as u32).div_ceil(256), 1, 1],
            "node.face_divergence",
        );
    }
}
