//! `node.particle_distance` — the liquid's signed distance at cell centres,
//! from its particles (docs/GPU_FLIP_PRESSURE_SOLVE.md section 1 (the step)):
//! the level set the pressure solve reads to place the free surface between
//! cell centres. A per-element gather on the codegen path.
//!
//! Ported from FLIP Fluids particlelevelset.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md
//!
//! The engine scatters |c − p| − r into every cell of the box 2r around each
//! particle (per axis, floor((p ± 2r − min) / h)), over a field filled with
//! 3h, then snaps |φ| < 0.005h to ±0.005h. r is the engine's √3·h/2 at its
//! default scale, so 2r < 2h: the box never leaves the 5 × 5 × 5 bins around
//! the particle's own, and gathering over those bins with the same box test
//! gives the same field. At that radius every cell holding a particle reads
//! φ ≤ −0.005h, which is what the ghost rows need of a water cell. A cell
//! holding no particle can read φ < 0 too (the engine would call it liquid);
//! the ghost rows take an air side's φ at least 0 (docs/GPU_FLIP_PRESSURE_SOLVE.md
//! section 2 (the equation)). Deviations: a cell with no live particle in
//! the 27 cells around it stays 3h, where the engine can read down to
//! 1.5h − r from a particle two cells out (the solve reads φ only at water
//! cells and their neighbours, which always have one; the outer ring is then
//! read only where it can lower φ); solids are not pushed below −h/2 (the
//! solve has no solids).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::{LATTICE_PARAMS, cell_capacity, cell_count, cell_lattice};
use super::particles_to_faces::lattice_box;
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::{CellRange, FluidParticle};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DistanceUniforms {
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
    name: ParticleDistance,
    type_id: "node.particle_distance",
    purpose: "Signed distance from the liquid's particles at each cell centre of a lattice (nodes_x/y/z cells of cell_size from lattice_min, cell (i, j, k) at i + nx·(j + ny·k), centre lattice_min + (i + ½, j + ½, k + ½)·cell_size). Each particle is a ball of radius r = √3·cell_size / 2 (half a cell's diagonal): out = min(3·cell_size, min of |centre − position| − r over live particles (radius > 0) within 2r of the cell along each axis, by cell index: floor((position ± 2r − lattice_min) / cell_size)), negative inside the liquid; a cell with no live particle in itself or its 26 neighbours reads 3·cell_size. A value within 0.005·cell_size of zero becomes ±0.005·cell_size, keeping its sign (zero becomes negative). A cell holding a live particle always reads −0.005·cell_size or less; an empty cell next to one can read below zero too. Cells past the lattice are zero.",
    inputs: {
        sorted: Array(FluidParticle) required,
        cell_ranges: Array(CellRange) required,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
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
        float_param!("lattice_min_x", "Lattice Min X", -2.0, -1.0e4, 1.0e4),
        float_param!("lattice_min_y", "Lattice Min Y", 0.0, -1.0e4, 1.0e4),
        float_param!("lattice_min_z", "Lattice Min Z", -2.0, -1.0e4, 1.0e4),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.sort_particles_into_cells binned by the same lattice (box = the lattice, cell_size its cell): wire its sorted and cell_ranges. Feeds the phi input of node.pressure_smooth, node.pressure_residual and node.subtract_pressure on the finest level, which place the free surface where this crosses zero instead of at the air cells' centres.",
    examples: [],
    picker: { label: "Particle Distance", category: Atom },
    summary: "Measures how far each grid cell is from the liquid, so the solver knows where its surface sits.",
    category: Particles3D,
    role: Filter,
    aliases: ["level set", "signed distance", "liquid sdf", "free surface", "phi"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/particle_distance_body.wgsl"),
    input_access: [BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::ParamProduct { params: &LATTICE_PARAMS, plus: 0 },
}

impl Primitive for ParticleDistance {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| cell_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Particle Distance: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let (min, cell_size) = lattice_box(ctx);
        if !(cell_size.is_finite() && cell_size > 0.0) || min.iter().any(|v| !v.is_finite()) {
            ctx.error("Particle Distance: the lattice box must be finite with a positive cell".to_string());
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(sorted), Some(ranges), Some(out)) =
            (ctx.inputs.array("sorted"), ctx.inputs.array("cell_ranges"), ctx.outputs.array("out"))
        else {
            return;
        };
        let cells = cell_count(nodes);
        let range_size = std::mem::size_of::<CellRange>() as u64;
        let particle_size = std::mem::size_of::<FluidParticle>() as u64;
        if cells * range_size > ranges.size || cells * 4 > out.size || sorted.size < particle_size {
            ctx.error(format!("Particle Distance: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = DistanceUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            cell_size,
            lattice_min_x: min[0],
            lattice_min_y: min[1],
            lattice_min_z: min[2],
            dispatch_count: cells as u32,
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
            [(cells as u32).div_ceil(256), 1, 1],
            "node.particle_distance",
        );
    }
}
