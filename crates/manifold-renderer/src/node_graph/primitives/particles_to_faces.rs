//! `node.particles_to_faces` — particles to the face grid by gather
//! (docs/FFT_WATER_SOLVER_DESIGN.md D2, D7): each face reads the particles
//! around it through the sort's cell ranges and sums tent weights and
//! momentum itself, so no atomics exist. A per-element gather on the codegen
//! path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::collar_cells::{cell_count, cell_lattice};
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::{CellRange, FaceSample, FluidParticle};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Padded cells of the face grid: one more than the lattice per axis.
pub(super) fn face_count(nodes: [u32; 3]) -> u64 {
    nodes.iter().map(|&n| u64::from(n) + 1).product()
}

/// Face-grid length for a lattice param set, for `array_output_capacity`.
pub(super) fn face_capacity(params: &ParamValues) -> Option<u32> {
    cell_lattice(params).and_then(|nodes| u32::try_from(face_count(nodes)).ok())
}

/// The lattice box of the particle atoms: its minimum corner and cell size.
pub(super) fn lattice_box(ctx: &EffectNodeContext<'_, '_>) -> ([f32; 3], f32) {
    let min = [("lattice_min_x", -2.0), ("lattice_min_y", 0.0), ("lattice_min_z", -2.0)]
        .map(|(name, default)| ctx.scalar_or_param(name, default));
    (min, ctx.scalar_or_param("cell_size", 0.0625))
}

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FacesUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: ParticlesToFaces,
    type_id: "node.particles_to_faces",
    purpose: "Transfer liquid particles to a face grid (velocity on cell faces). The lattice has nodes_x/y/z cells of cell_size from lattice_min; out has (nodes + 1)³ padded cells, padded cell (i, j, k) at i + (nx + 1)·(j + (ny + 1)·k) owning the x face at (i, j + ½, k + ½)·h, the y face at (i + ½, j, k + ½)·h and the z face at (i + ½, j + ½, k)·h. Each face's weight is the sum over live particles (radius > 0) of Π max(0, 1 − |Δ|/h), and its velocity the weighted mean of the particles' velocity along the face normal (0 with no weight). Faces past the lattice are zero.",
    inputs: {
        sorted: Array(FluidParticle) required,
        cell_ranges: Array(CellRange) required,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
    },
    outputs: {
        out: Array(FaceSample),
    },
    params: [
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        float_param!("lattice_min_x", "Lattice Min X", -2.0, -1.0e4, 1.0e4),
        float_param!("lattice_min_y", "Lattice Min Y", 0.0, -1.0e4, 1.0e4),
        float_param!("lattice_min_z", "Lattice Min Z", -2.0, -1.0e4, 1.0e4),
    ],
    depth_rule: Terminal,
    composition_notes: "The first step of the FFT water step, after node.sort_particles_into_cells binned by the same lattice (box = the lattice, cell_size its cell): wire its sorted and cell_ranges. Feeds node.face_gravity; keep this output as the FLIP reference for node.faces_to_particles' old input.",
    examples: [],
    picker: { label: "Particles To Faces", category: Atom },
    summary: "Spreads the liquid particles' motion onto a grid so the solver can make it incompressible.",
    category: Particles3D,
    role: Filter,
    aliases: ["particle to grid", "p2g", "face velocity", "mac grid"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/particles_to_faces_body.wgsl"),
    input_access: [BufferGather, BufferGather],
}

impl Primitive for ParticlesToFaces {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| face_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Particles To Faces: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let (min, cell_size) = lattice_box(ctx);
        if !(cell_size.is_finite() && cell_size > 0.0) || min.iter().any(|v| !v.is_finite()) {
            ctx.error("Particles To Faces: the lattice box must be finite with a positive cell".to_string());
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(sorted), Some(ranges), Some(out)) =
            (ctx.inputs.array("sorted"), ctx.inputs.array("cell_ranges"), ctx.outputs.array("out"))
        else {
            return;
        };
        let faces = face_count(nodes);
        let range_size = std::mem::size_of::<CellRange>() as u64;
        if cell_count(nodes) * range_size > ranges.size || faces * 32 > out.size || sorted.size < 32 {
            ctx.error(format!("Particles To Faces: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = FacesUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            cell_size,
            lattice_min_x: min[0],
            lattice_min_y: min[1],
            lattice_min_z: min[2],
            dispatch_count: faces as u32,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: sorted, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: ranges, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: out, offset: 0 },
            ],
            [(faces as u32).div_ceil(256), 1, 1],
            "node.particles_to_faces",
        );
    }
}
