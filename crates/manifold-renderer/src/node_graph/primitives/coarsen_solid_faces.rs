//! `node.coarsen_solid_faces` — the face open fractions of a multigrid
//! level half as long on every axis (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8
//! (solids in the water)): each coarse face is the mean of the four fine
//! faces it covers. A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::{LATTICE_PARAMS, cell_lattice};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use super::particles_to_faces::{face_capacity, face_count};
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CoarsenFacesUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: CoarsenSolidFaces,
    type_id: "node.coarsen_solid_faces",
    purpose: "Halve a face grid's open fractions on every axis (node.particles_to_faces' layout; nodes_x/y/z are the coarse cells, the fine grid has twice as many per axis): each coarse face's weight is the mean of the four fine face weights it covers. Box wall faces and faces past the lattice are 0. Velocity is 0.",
    inputs: {
        fine: Array(FaceSample) required,
    },
    outputs: {
        out: Array(FaceSample),
    },
    params: [
        float_param!("nodes_x", "Coarse Cells X", 32.0, 1.0, 512.0),
        float_param!("nodes_y", "Coarse Cells Y", 32.0, 1.0, 512.0),
        float_param!("nodes_z", "Coarse Cells Z", 32.0, 1.0, 512.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Chained from node.solid_faces once per water step, one per multigrid level beside node.coarsen_water: the level's node.pressure_smooth, node.pressure_residual and, at the coarsest, node.coarse_inverse read it.",
    examples: [],
    picker: { label: "Coarsen Solid Faces", category: Atom },
    summary: "Makes a half-size copy of how open each grid face is, for the pressure solver's coarse levels.",
    category: Particles3D,
    role: Filter,
    aliases: ["coarse face weights", "restrict weights", "multigrid weights"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/coarsen_solid_faces_body.wgsl"),
    input_access: [BufferGather],
    output_capacity: FusedOutputCapacity::ParamProduct { params: &LATTICE_PARAMS, plus: 1 },
}

impl Primitive for CoarsenSolidFaces {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| face_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Coarsen Solid Faces: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(fine), Some(out)) = (ctx.inputs.array("fine"), ctx.outputs.array("out")) else {
            return;
        };
        let faces = face_count(nodes);
        let record = size_of::<FaceSample>() as u64;
        if face_count(nodes.map(|n| 2 * n)) * record > fine.size || faces * record > out.size {
            ctx.error(format!("Coarsen Solid Faces: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = CoarsenFacesUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            dispatch_count: faces as u32,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: fine, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [(faces as u32).div_ceil(256), 1, 1],
            "node.coarsen_solid_faces",
        );
    }
}
