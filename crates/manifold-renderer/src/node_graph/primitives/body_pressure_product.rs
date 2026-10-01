//! Ported from FLIP Fluids pressuresolver.cpp and rigidboundaryvelocity.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
//!
//! `node.body_pressure_product` — the dynamic bodies' rows of the coupled
//! pressure operator (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (solids in
//! the water)): what a pressure's push on the bodies gives back to the
//! water's divergence. A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid::TICK;
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::liquid::bodies::{LiquidBody, SOLID_BODY_FACES};
use crate::node_graph::liquid::lattice::{cell_count, cell_lattice, face_count};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BodyProductUniforms {
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    body_count: f32,
    rows: f32,
    tick_seconds: f32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: BodyPressureProduct,
    type_id: "node.body_pressure_product",
    purpose: "Add the dynamic bodies' share of the pressure operator to base, per cell of a lattice (nodes_x/y/z cells from lattice_min, cell_size apart). In a water cell: base + (1/cell_size)·Σ over the cell's inner faces a body owns (solid_velocity's velocity w carries the owner code; no node writes it yet, BUG-6zj3 (step body owner code)) of ±(c − o)·(dv + dω × r) along the face's axis, + on the cell's high faces and − on its low, c the cell's open volume and o the face's open fraction from solid_faces, r from the body's centre of mass (bodies, posed tick_seconds on) to the face centre, dv and dω the body's velocity change in sums (node.face_impulse_to_bodies). Air cells keep base. As FLIP Fluids' coupled matrix adds J M⁻¹ Jᵀ.",
    inputs: {
        base: Array(f32) required,
        water: Array(f32) required,
        solid_faces: Array(FaceSample) required,
        solid_velocity: Array(FaceSample) required,
        sums: Array(f32) required,
        bodies: Array(LiquidBody) required,
        body_count: ScalarF32 optional,
        rows: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("lattice_min_x", "Lattice Min X", -2.0, -1000.0, 1000.0),
        float_param!("lattice_min_y", "Lattice Min Y", 0.0, -1000.0, 1000.0),
        float_param!("lattice_min_z", "Lattice Min Z", -2.0, -1000.0, 1000.0),
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
        float_param!("body_count", "Bodies", 0.0, 0.0, MAX_FLUID_ROLES as f32),
        float_param!("rows", "Rows", 0.0, 0.0, 16_777_216.0),
        float_param!("tick_seconds", "Tick (s)", TICK as f32, 0.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The GPU FLIP two-way body coupling, not yet wired into any preset (BUG-6zj3 (step body owner code)): base is the fluid operator −L p on the pressure solve's search direction, sums are node.pressure_face_impulse then node.face_impulse_to_bodies on the same direction; its output is the solve's operator product. bodies, body_count and rows from the liquid's domain; solid_faces and solid_velocity are the step's.",
    examples: [],
    picker: { label: "Body Pressure Product", category: Atom },
    summary: "Lets floating objects give way to the water's pressure inside the pressure solve, so heavy and light objects float and sink correctly.",
    category: Particles3D,
    role: Filter,
    aliases: ["body coupling", "two-way coupling", "rigid coupling", "buoyancy"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/body_pressure_product_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "base" },
    wgsl_includes: [SOLID_BODY_FACES],
}

impl Primitive for BodyPressureProduct {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "base").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Body Pressure Product: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let min = ["lattice_min_x", "lattice_min_y", "lattice_min_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let body_count = ctx.scalar_or_param("body_count", 0.0);
        let rows = ctx.scalar_or_param("rows", 0.0);
        let tick_seconds = ctx.scalar_or_param("tick_seconds", TICK as f32);
        if !(cell_size.is_finite() && cell_size > 0.0 && min.iter().all(|v| v.is_finite()) && tick_seconds.is_finite()) {
            ctx.error("Body Pressure Product: cell_size must be positive and the lattice and tick finite".to_string());
            return;
        }
        if !(body_count >= 0.0 && body_count <= MAX_FLUID_ROLES as f32 && rows >= 0.0 && rows.fract() == 0.0 && body_count.fract() == 0.0) {
            ctx.error(format!("Body Pressure Product: body_count {body_count} and rows {rows} must be whole, body_count at most {MAX_FLUID_ROLES}"));
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(base), Some(water), Some(solid_faces), Some(solid_velocity), Some(sums), Some(bodies), Some(out)) = (
            ctx.inputs.array("base"),
            ctx.inputs.array("water"),
            ctx.inputs.array("solid_faces"),
            ctx.inputs.array("solid_velocity"),
            ctx.inputs.array("sums"),
            ctx.inputs.array("bodies"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let cells = cell_count(nodes);
        let record = size_of::<FaceSample>() as u64;
        if cells * 4 > base.size.min(water.size).min(out.size) || face_count(nodes) * record > solid_faces.size.min(solid_velocity.size) {
            ctx.error(format!("Body Pressure Product: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = BodyProductUniforms {
            lattice_min_x: min[0],
            lattice_min_y: min[1],
            lattice_min_z: min[2],
            cell_size,
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            body_count,
            rows,
            tick_seconds,
            dispatch_count: cells as u32,
            _pad0: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: base, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: water, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: solid_faces, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: solid_velocity, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: sums, offset: 0 },
                GpuBinding::Buffer { binding: 6, buffer: bodies, offset: 0 },
                GpuBinding::Buffer { binding: 7, buffer: out, offset: 0 },
            ],
            [(cells as u32).div_ceil(256), 1, 1],
            "node.body_pressure_product",
        );
    }
}
