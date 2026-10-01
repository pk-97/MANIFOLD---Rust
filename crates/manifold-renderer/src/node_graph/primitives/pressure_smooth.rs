//! `node.pressure_smooth` — one red-black Gauss-Seidel sweep of the water's
//! pressure equation, the smoother of the multigrid pressure solve
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md). A per-element gather on the codegen
//! path: a sweep updates one color from the other, so one dispatch is exact.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::{cell_count, cell_lattice};
use super::particles_to_faces::face_count;
use super::sort_particles_into_cells::{float_param, int_param};
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SmoothUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    color: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: PressureSmooth,
    type_id: "node.pressure_smooth",
    purpose: "One red-black Gauss-Seidel sweep of the weighted Poisson equation L p = rhs on a lattice (nodes_x/y/z cells, cell (i, j, k) at i + nx·(j + ny·k)), each face weighted by its open fraction w from solid_faces (node.solid_faces' face grid; box walls 0): each water cell (water > 0.5) of the swept color, (i + j + k) mod 2 = color, becomes (Σ w · its water neighbours' value − cell_size² · rhs) / (Σ w over its faces). Air neighbours hold zero pressure. A water cell with no open face is out of the system and becomes 0. Every other cell keeps its value.",
    inputs: {
        water: Array(f32) required,
        rhs: Array(f32) required,
        value: Array(f32) required,
        solid_faces: Array(FaceSample) required,
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
        int_param!("color", "Color", 0.0, 0.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The smoother of the water's multigrid pressure solve. A full sweep is two nodes, color 0 then color 1 (the other order after the coarse correction, so the V-cycle stays symmetric). The first sweep of a V-cycle level starts from zeros: wire value from node.array_math (ScaleOffset, scale 0) of that level's water. water is node.cells_with_particles or node.coarsen_water; rhs the level's residual.",
    examples: [],
    picker: { label: "Smooth Pressure", category: Atom },
    summary: "Evens out the water's pressure one checkerboard color at a time.",
    category: Particles3D,
    role: Filter,
    aliases: ["gauss seidel", "red black", "relax", "smoother", "multigrid smooth"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/pressure_smooth_body.wgsl"),
    input_access: [BufferGather, Coincident, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "rhs" },
}

/// The sweep's color: 0 or 1.
fn color(params: &ParamValues) -> i32 {
    match params.get("color") {
        Some(ParamValue::Float(v)) => i32::from(v.round() >= 0.5),
        _ => 0,
    }
}

impl Primitive for PressureSmooth {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "rhs").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Smooth Pressure: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        if !(cell_size.is_finite() && cell_size > 0.0) {
            ctx.error("Smooth Pressure: cell_size must be positive".to_string());
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(water), Some(rhs), Some(value), Some(solid_faces), Some(out)) = (
            ctx.inputs.array("water"),
            ctx.inputs.array("rhs"),
            ctx.inputs.array("value"),
            ctx.inputs.array("solid_faces"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let cells = cell_count(nodes);
        if cells * 4 > water.size.min(rhs.size).min(value.size).min(out.size)
            || face_count(nodes) * size_of::<FaceSample>() as u64 > solid_faces.size
        {
            ctx.error(format!("Smooth Pressure: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = SmoothUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            cell_size,
            color: color(ctx.params),
            dispatch_count: cells as u32,
            _pad0: 0,
            _pad1: 0,
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
                GpuBinding::Buffer { binding: 5, buffer: out, offset: 0 },
            ],
            [(cells as u32).div_ceil(256), 1, 1],
            "node.pressure_smooth",
        );
    }
}
