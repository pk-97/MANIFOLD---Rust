//! Ported from FLIP Fluids rigidboundaryvelocity.cpp and pressuresolver.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
//!
//! `node.pressure_face_impulse` — the pressure's push on the bodies, face by
//! face (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (solids in the water)).
//! A per-element gather on the codegen path; node.face_impulse_to_bodies sums
//! it per body.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::liquid::bodies::SOLID_BODY_FACES;
use crate::node_graph::liquid::lattice::{LATTICE_PARAMS, cell_count, cell_lattice, face_capacity, face_count};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FaceImpulseUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    density: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: PressureFaceImpulse,
    type_id: "node.pressure_face_impulse",
    purpose: "The pressure's impulse on the bodies through each face of a face grid (the liquid face grid's layout). On an inner face a body owns (solid_velocity's velocity w carries the owner code, Σ over axes a of (body_a + 1)·256^a; no node writes it yet, BUG-6zj3 (step body owner code)), the face's axis carries density·cell_size²·((c_lo − o)·p_lo − (c_hi − o)·p_hi) in N·s: o the face's open fraction and c each side cell's open volume from solid_faces, p the pressure of each side cell (0 outside the water) in the solve's units, dt·P/ρ. FLIP Fluids' forcePerPressure times the pressure. Every other face is zero; velocity w passes the owner code on.",
    inputs: {
        pressure: Array(f32) required,
        water: Array(f32) required,
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
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        float_param!("density", "Liquid Density (kg/m³)", 1000.0, 1.0e-3, 1.0e6),
    ],
    depth_rule: Terminal,
    composition_notes: "The GPU FLIP two-way body coupling, not yet wired into any preset (BUG-6zj3 (step body owner code)): on the pressure solve's search direction, then node.face_impulse_to_bodies, then node.body_pressure_product adds the bodies' share of the operator; and once on the solved pressure for the tick's reaction. water is the step's water mask; solid_faces and solid_velocity are the step's open fractions and solid face velocity.",
    examples: [],
    picker: { label: "Pressure Face Impulse", category: Atom },
    summary: "Works out how hard the water's pressure pushes on floating objects through each face of the grid.",
    category: Particles3D,
    role: Filter,
    aliases: ["pressure force", "buoyancy", "body coupling", "two-way coupling"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/pressure_face_impulse_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, Coincident],
    output_capacity: FusedOutputCapacity::ParamProduct { params: &LATTICE_PARAMS, plus: 1 },
    wgsl_includes: [SOLID_BODY_FACES],
}

impl Primitive for PressureFaceImpulse {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| face_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Pressure Face Impulse: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let density = ctx.scalar_or_param("density", 1000.0);
        if !(cell_size.is_finite() && cell_size > 0.0 && density.is_finite() && density > 0.0) {
            ctx.error("Pressure Face Impulse: cell_size and density must be positive".to_string());
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(pressure), Some(water), Some(solid_faces), Some(solid_velocity), Some(out)) = (
            ctx.inputs.array("pressure"),
            ctx.inputs.array("water"),
            ctx.inputs.array("solid_faces"),
            ctx.inputs.array("solid_velocity"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let faces = face_count(nodes);
        let record = size_of::<FaceSample>() as u64;
        if cell_count(nodes) * 4 > pressure.size.min(water.size)
            || faces * record > solid_faces.size.min(solid_velocity.size).min(out.size)
        {
            ctx.error(format!("Pressure Face Impulse: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = FaceImpulseUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            cell_size,
            density,
            dispatch_count: faces as u32,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: pressure, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: water, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: solid_faces, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: solid_velocity, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: out, offset: 0 },
            ],
            [(faces as u32).div_ceil(256), 1, 1],
            "node.pressure_face_impulse",
        );
    }
}
