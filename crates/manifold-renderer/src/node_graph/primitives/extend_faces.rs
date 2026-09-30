//! `node.extend_faces` — one layer of velocity extension into invalid faces
//! (docs/FFT_WATER_SOLVER_DESIGN.md section 3 step 8), so particles near the
//! surface sample only meaningful faces. A per-element gather on the codegen
//! path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::cell_lattice;
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
struct ExtendUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: ExtendFaces,
    type_id: "node.extend_faces",
    purpose: "Extend a face grid (node.particles_to_faces' layout) by one layer: a face with weight > 0 is copied unchanged; one without takes the mean velocity of the same component's faces with weight > 0 among its six neighbours and gets weight 1; with none it is left as it was.",
    inputs: {
        faces: Array(FaceSample) required,
    },
    outputs: {
        out: Array(FaceSample),
    },
    params: [
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Chain one per layer after node.subtract_pressure (and after node.particles_to_faces for the FLIP reference): the particles' RK3 steps reach about one cell per step past the water, so two layers cover a step that moves under a cell.",
    examples: [],
    picker: { label: "Extend Face Velocity", category: Atom },
    summary: "Carries the liquid's motion one cell out into the air, so particles at the surface move smoothly.",
    category: Particles3D,
    role: Filter,
    aliases: ["extrapolate velocity", "velocity extension"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/extend_faces_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for ExtendFaces {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| face_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Extend Face Velocity: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(faces), Some(out)) = (ctx.inputs.array("faces"), ctx.outputs.array("out")) else {
            return;
        };
        let count = face_count(nodes);
        if count * 32 > faces.size.min(out.size) {
            ctx.error(format!("Extend Face Velocity: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms =
            ExtendUniforms { nodes_x: nodes[0] as f32, nodes_y: nodes[1] as f32, nodes_z: nodes[2] as f32, dispatch_count: count as u32 };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: faces, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.extend_faces",
        );
    }
}
