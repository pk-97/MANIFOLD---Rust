//! Ported from FLIP Fluids fluidsimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
//!
//! `node.constrain_solid_faces` — the solids' velocity onto the faces they
//! close or cut, after the pressure step (docs/GPU_FLIP_PRESSURE_SOLVE.md
//! section 8 (solids in the water)). A per-element atom on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::{LATTICE_PARAMS, cell_lattice};
use super::particles_to_faces::{face_capacity, face_count};
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ConstrainUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: ConstrainSolidFaces,
    type_id: "node.constrain_solid_faces",
    purpose: "Put the solids' velocity on a face grid (node.particles_to_faces' layout): on an inner face with open fraction w (solid_faces, node.solid_faces) and solid velocity v_s and friction f (solid_velocity, node.solid_face_velocity), a closed face (w = 0) takes v_s, a cut face (0 < w < 1) takes f·v_s + (1 − f)·u, an open face keeps u, as FLIP Fluids constrains its velocity field. Box wall faces and the weights pass through.",
    inputs: {
        faces: Array(FaceSample) required,
        solid_faces: Array(FaceSample) required,
        solid_velocity: Array(FaceSample) required,
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
    composition_notes: "In a GPU FLIP water step after node.subtract_pressure and before node.extend_faces, and on the step's starting faces before node.faces_to_particles reads them as old, as the engine constrains both its velocity and its saved velocity after the pressure solve.",
    examples: [],
    picker: { label: "Constrain Solid Faces", category: Atom },
    summary: "Makes the water move with solid objects where it touches them.",
    category: Particles3D,
    role: Filter,
    aliases: ["solid boundary", "no slip", "friction", "obstacle velocity"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/constrain_solid_faces_body.wgsl"),
    input_access: [Coincident, Coincident, Coincident],
    output_capacity: FusedOutputCapacity::ParamProduct { params: &LATTICE_PARAMS, plus: 1 },
}

impl Primitive for ConstrainSolidFaces {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| face_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Constrain Solid Faces: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(faces), Some(solid_faces), Some(solid_velocity), Some(out)) = (
            ctx.inputs.array("faces"),
            ctx.inputs.array("solid_faces"),
            ctx.inputs.array("solid_velocity"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let count = face_count(nodes);
        if count * size_of::<FaceSample>() as u64 > faces.size.min(solid_faces.size).min(solid_velocity.size).min(out.size) {
            ctx.error(format!("Constrain Solid Faces: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = ConstrainUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            dispatch_count: count as u32,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: faces, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: solid_faces, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: solid_velocity, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.constrain_solid_faces",
        );
    }
}
