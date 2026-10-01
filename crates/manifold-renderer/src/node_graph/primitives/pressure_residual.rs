//! `node.pressure_residual` — what the water's pressure equation still
//! misses, rhs − L p, on water cells (docs/GPU_FLIP_PRESSURE_SOLVE.md): the
//! residual a multigrid level hands down, and, with rhs zero, −L applied to a
//! search direction. A per-element gather on the codegen path.
//!
//! Ported from FLIP Fluids pressuresolver.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md
//!
//! The same rows as `node.pressure_smooth` (the engine's
//! `_calculateMatrixCoefficientsThread`), with the same deviations.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::{cell_count, cell_lattice};
use super::particles_to_faces::face_count;
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResidualUniforms {
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
    name: PressureResidual,
    type_id: "node.pressure_residual",
    purpose: "The residual of the weighted ghost-fluid Poisson equation on a lattice (nodes_x/y/z cells, cell (i, j, k) at i + nx·(j + ny·k)): out = rhs − L value in water cells (water > 0.5), 0 in air, where L value = (Σ w · the water neighbours' value − diag · value) / cell_size², w each face's open fraction from solid_faces (node.solid_faces' face grid; box walls 0). diag is Σ w over the cell's faces plus, for each air neighbour a, −w · clamp(max(phi_a, 0) / min(phi_c, −0.005·cell_size), −25, 25): node.pressure_smooth's ghost-fluid rows. With phi all zero, air holds zero pressure at its cells' centres. A water cell with no open face is out of the system: 0.",
    inputs: {
        water: Array(f32) required,
        rhs: Array(f32) required,
        value: Array(f32) required,
        solid_faces: Array(FaceSample) required,
        phi: Array(f32) required,
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
    composition_notes: "In a multigrid V-cycle, after the pre-smoothing node.pressure_smooth sweeps and before node.restrict_lattice. In the conjugate gradient loop, with rhs zero (node.array_math ScaleOffset, scale 0, of the water), it applies −L to the search direction. Wire phi as the level's node.pressure_smooth sweeps are wired.",
    examples: [],
    picker: { label: "Pressure Residual", category: Atom },
    summary: "Measures how far the water's pressure is from balancing its flow, cell by cell.",
    category: Particles3D,
    role: Filter,
    aliases: ["residual", "laplacian", "poisson error", "multigrid residual"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/pressure_residual_body.wgsl"),
    input_access: [BufferGather, Coincident, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "rhs" },
}

impl Primitive for PressureResidual {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "rhs").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Pressure Residual: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        if !(cell_size.is_finite() && cell_size > 0.0) {
            ctx.error("Pressure Residual: cell_size must be positive".to_string());
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(water), Some(rhs), Some(value), Some(solid_faces), Some(phi), Some(out)) = (
            ctx.inputs.array("water"),
            ctx.inputs.array("rhs"),
            ctx.inputs.array("value"),
            ctx.inputs.array("solid_faces"),
            ctx.inputs.array("phi"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let cells = cell_count(nodes);
        if cells * 4 > water.size.min(rhs.size).min(value.size).min(phi.size).min(out.size)
            || face_count(nodes) * size_of::<FaceSample>() as u64 > solid_faces.size
        {
            ctx.error(format!("Pressure Residual: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = ResidualUniforms {
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
                GpuBinding::Buffer { binding: 1, buffer: water, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: rhs, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: value, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: solid_faces, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: phi, offset: 0 },
                GpuBinding::Buffer { binding: 6, buffer: out, offset: 0 },
            ],
            [(cells as u32).div_ceil(256), 1, 1],
            "node.pressure_residual",
        );
    }
}
