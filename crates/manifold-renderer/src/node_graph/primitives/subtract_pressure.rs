//! `node.subtract_pressure` — the projection (docs/FFT_WATER_SOLVER_DESIGN.md
//! section 3 step 8): faces touching water lose the pressure gradient, which
//! leaves the water's velocity without divergence. A per-element gather on
//! the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::collar_cells::{cell_count, cell_lattice};
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
struct SubtractUniforms {
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
    name: SubtractPressure,
    type_id: "node.subtract_pressure",
    purpose: "Apply a pressure field to a face grid (node.particles_to_faces' layout). Box wall faces become 0 and valid; an inner face with water on either side becomes u − (p_upper − p_lower) / cell_size and valid; a face between two air cells keeps u and is marked invalid. The output weight is 1 for valid, 0 for invalid. pressure is 0 in air.",
    inputs: {
        faces: Array(FaceSample) required,
        pressure: Array(f32) required,
        water: Array(f32) required,
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
    ],
    depth_rule: Terminal,
    composition_notes: "faces is node.face_gravity's output (the field node.face_divergence measured), pressure the FFT water solve's node.collar_pressure, water node.cells_with_particles. Follow with node.extend_faces so particles near the surface read valid faces.",
    examples: [],
    picker: { label: "Subtract Pressure", category: Atom },
    summary: "Uses the pressure to push the liquid so it neither squashes nor stretches.",
    category: Particles3D,
    role: Filter,
    aliases: ["projection", "pressure gradient", "make incompressible"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/subtract_pressure_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather],
}

impl Primitive for SubtractPressure {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| face_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Subtract Pressure: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        if !(cell_size.is_finite() && cell_size > 0.0) {
            ctx.error("Subtract Pressure: cell_size must be positive".to_string());
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(faces), Some(pressure), Some(water), Some(out)) = (
            ctx.inputs.array("faces"),
            ctx.inputs.array("pressure"),
            ctx.inputs.array("water"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let count = face_count(nodes);
        if count * 32 > faces.size.min(out.size) || cell_count(nodes) * 4 > pressure.size.min(water.size) {
            ctx.error(format!("Subtract Pressure: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = SubtractUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            cell_size,
            dispatch_count: count as u32,
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
                GpuBinding::Buffer { binding: 2, buffer: pressure, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: water, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.subtract_pressure",
        );
    }
}
