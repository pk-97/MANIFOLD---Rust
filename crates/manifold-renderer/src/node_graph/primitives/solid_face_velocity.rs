//! Ported from FLIP Fluids fluidsimulation.cpp and meshlevelset.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
//!
//! `node.solid_face_velocity` — the solids' velocity and friction on the
//! faces they cut (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (solids in the
//! water)): what a moving body pushes into the water's divergence and leaves
//! on its closed faces. A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::{LATTICE_PARAMS, cell_lattice};
use super::particles_to_faces::{face_capacity, face_count};
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid::TICK;
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SolidFaceVelocityUniforms {
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
    name: SolidFaceVelocity,
    type_id: "node.solid_face_velocity",
    purpose: "The solids' velocity and friction on a face grid (node.particles_to_faces' layout, nodes_x/y/z cells from lattice_min, cell_size apart). On an inner face a solid cuts (open fraction under 1 in solid_faces, node.solid_faces' face grid), velocity is the closest body's rigid velocity at the face centre along the face's axis, and weight is the friction of the closest body at each of the face's four corners, averaged (0 at a corner no body reaches), as FLIP Fluids takes its solid face velocity and friction. Every other face is zero. Bodies are posed after tick_seconds, as node.liquid_solid_distance poses them. A dynamic body (1/m above 0) moves at its predicted velocity: its velocity plus its acceleration times tick_seconds plus its velocity change so far this tick from changes (node.face_impulse_to_bodies' sums, 16 floats per body). Velocity w carries each face's owner, Σ over the axes a of (b + 1)·256^a, b the owning body from body 0 or −1 for none.",
    inputs: {
        solid_faces: Array(FaceSample) required,
        bodies: Array(LiquidBody) required,
        shapes: Array(LiquidShape) required,
        atlas: Array(u32) required,
        changes: Array(f32) required,
        body_count: ScalarF32 optional,
        rows: ScalarF32 optional,
    },
    outputs: {
        out: Array(FaceSample),
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
    composition_notes: "Once per GPU FLIP water step, beside node.solid_faces with the same pose. bodies, shapes, atlas, body_count and rows come from the liquid's domain (node.gpu_flip_domain's body_rows as rows). Feeds node.face_divergence's solid_velocity and node.constrain_solid_faces.",
    examples: [],
    picker: { label: "Solid Face Velocity", category: Atom },
    summary: "Works out how fast solid objects move where they touch the water's grid, so a moving box pushes the water.",
    category: Particles3D,
    role: Filter,
    aliases: ["solid velocity", "obstacle velocity", "moving solid", "boundary velocity"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/solid_face_velocity_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::ParamProduct { params: &LATTICE_PARAMS, plus: 1 },
    wgsl_includes: [LIQUID_POSE, LIQUID_COLLIDER],
}

impl Primitive for SolidFaceVelocity {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| face_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Solid Face Velocity: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let min = ["lattice_min_x", "lattice_min_y", "lattice_min_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let body_count = ctx.scalar_or_param("body_count", 0.0);
        let rows = ctx.scalar_or_param("rows", 0.0);
        let tick_seconds = ctx.scalar_or_param("tick_seconds", TICK as f32);
        if !(cell_size.is_finite() && cell_size > 0.0 && min.iter().all(|v| v.is_finite()) && tick_seconds.is_finite()) {
            ctx.error("Solid Face Velocity: cell_size must be positive and the lattice and tick finite".to_string());
            return;
        }
        if !(body_count >= 0.0 && body_count <= MAX_FLUID_ROLES as f32 && rows >= 0.0 && rows.fract() == 0.0 && body_count.fract() == 0.0) {
            ctx.error(format!("Solid Face Velocity: body_count {body_count} and rows {rows} must be whole, body_count at most {MAX_FLUID_ROLES}"));
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(solid_faces), Some(bodies), Some(shapes), Some(atlas), Some(changes), Some(out)) = (
            ctx.inputs.array("solid_faces"),
            ctx.inputs.array("bodies"),
            ctx.inputs.array("shapes"),
            ctx.inputs.array("atlas"),
            ctx.inputs.array("changes"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let faces = face_count(nodes);
        let record = size_of::<FaceSample>() as u64;
        if faces * record > solid_faces.size.min(out.size) {
            ctx.error(format!("Solid Face Velocity: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        if rows as u64 * size_of::<LiquidBody>() as u64 > bodies.size {
            ctx.error(format!("Solid Face Velocity: {rows} body rows are more than the bodies array holds"));
            return;
        }
        let uniforms = SolidFaceVelocityUniforms {
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
            dispatch_count: faces as u32,
            _pad0: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: solid_faces, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: bodies, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: shapes, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: atlas, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: changes, offset: 0 },
                GpuBinding::Buffer { binding: 6, buffer: out, offset: 0 },
            ],
            [(faces as u32).div_ceil(256), 1, 1],
            "node.solid_face_velocity",
        );
    }
}
