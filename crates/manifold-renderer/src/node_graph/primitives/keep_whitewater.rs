//! `node.keep_whitewater` — FLIP's whitewater removal as a keep flag per pool
//! slot (`docs/GPU_WHITEWATER_DESIGN.md` section 3.9, phase L4). A
//! per-element gather on the codegen path; `node.running_total` over the
//! flags and `node.compact_whitewater` finish the removal.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::{bin_param, float_param};
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::{CellRange, bin_counts, searched_bins};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{WHITEWATER_COMMON, WhitewaterParticle, cell_total, particle_grid};

/// FLIP's `_maxDiffuseParticlesPerCell`.
pub(crate) const MAX_PER_CELL: f32 = 5000.0;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct KeepUniforms {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cap: f32,
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

const _: () = assert!(std::mem::size_of::<KeepUniforms>() == 64);

crate::primitive! {
    name: KeepWhitewater,
    type_id: "node.keep_whitewater",
    purpose: "Which whitewater pool slots survive the tick, FLIP's removal with every side colliding: 1 to keep, 0 to remove. A slot goes when it is empty or the pool's header (kind 3), its lifetime is at or below 0, its position is not finite, it lies outside FLIP's boundary box (1.625 cells in from the whitewater grid) or inside the solid, or its cell already holds Max Per Cell kept particles earlier in the pool. One u32 per slot.",
    inputs: {
        pool: Array(WhitewaterParticle) required,
        binned: Array(WhitewaterParticle) required,
        cell_ranges: Array(CellRange) required,
        order: Array(u32) required,
        solid: Array(f32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        bins_x: ScalarF32 optional, bins_y: ScalarF32 optional, bins_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(u32),
    },
    params: [
        float_param!("center_x", "Grid Center X", 0.0, -1.0e4, 1.0e4),
        float_param!("center_y", "Grid Center Y", 0.0, -1.0e4, 1.0e4),
        float_param!("center_z", "Grid Center Z", 0.0, -1.0e4, 1.0e4),
        float_param!("size_x", "Grid Size X", 4.375, 0.0001, 1.0e4),
        float_param!("size_y", "Grid Size Y", 4.375, 0.0001, 1.0e4),
        float_param!("size_z", "Grid Size Z", 4.375, 0.0001, 1.0e4),
        float_param!("nodes_x", "Solid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 71.0, 3.0, 4096.0),
        float_param!("cap", "Max Per Cell", MAX_PER_CELL, 1.0, 1.0e6),
        bin_param!("bins_x", "Bins X"),
        bin_param!("bins_y", "Bins Y"),
        bin_param!("bins_z", "Bins Z"),
    ],
    depth_rule: Terminal,
    composition_notes: "The removal step at the end of the GPU whitewater tick, after node.preserve_foam. Wire pool and binned from the same pool, and that pool into a node.sort_particles_into_cells over the whitewater grid with the grid's cell as its cell size; cell_ranges, order and bins_x/y/z come from that sort. center/size/nodes_x/y/z and solid as node.advect_whitewater takes them. node.running_total over the flags, then node.compact_whitewater, drop the removed slots. While the sort has no lattice nothing is binned and the per-cell cap is not counted.",
    examples: [],
    picker: { label: "Keep Whitewater", category: Atom },
    summary: "Decides which foam, spray and bubbles survive this step: the dead, the stray and the overcrowded go.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater removal", "remove whitewater", "diffuse particle removal"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/keep_whitewater_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "pool" },
    wgsl_includes: [WHITEWATER_COMMON],
}

impl Primitive for KeepWhitewater {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "pool").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let grid = match particle_grid(ctx) {
            Ok(grid) => grid,
            Err(refusal) => {
                ctx.error(format!("Keep Whitewater: {refusal}"));
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(pool), Some(binned), Some(ranges), Some(order), Some(solid), Some(out)) = (
            ctx.inputs.array("pool"),
            ctx.inputs.array("binned"),
            ctx.inputs.array("cell_ranges"),
            ctx.inputs.array("order"),
            ctx.inputs.array("solid"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        if cell_total(grid.nodes) * 4 > solid.size {
            ctx.error(format!("Keep Whitewater: solid holds fewer than the {:?}-node lattice", grid.nodes));
            return;
        }
        let count = (pool.size / std::mem::size_of::<WhitewaterParticle>() as u64).min(out.size / 4) as u32;
        if count == 0 {
            return;
        }
        let ports = ["bins_x", "bins_y", "bins_z"];
        let wired = ports.map(|port| ctx.inputs.scalar(port).is_some());
        let set = ports.map(|port| ctx.scalar_or_param(port, 0.0));
        let bins = if set == [0.0; 3] && wired.iter().all(|&w| w) {
            // The sort has no lattice yet: nothing is binned.
            Ok([0; 3])
        } else if set == [0.0; 3] && wired.iter().all(|&w| !w) {
            let h = grid.size[0] / (grid.nodes[0] - 1) as f32;
            searched_bins(bin_counts(grid.size, h).map(|n| n as f32), ranges.size, "Keep Whitewater")
        } else {
            searched_bins(set, ranges.size, "Keep Whitewater")
        };
        let bins = match bins {
            Ok(bins) => bins.map(|n| n as i32),
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let [center_x, center_y, center_z] = grid.center;
        let [size_x, size_y, size_z] = grid.size;
        let [nodes_x, nodes_y, nodes_z] = grid.nodes.map(|n| n as f32);
        let uniforms = KeepUniforms {
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            nodes_x,
            nodes_y,
            nodes_z,
            cap: ctx.scalar_or_param("cap", MAX_PER_CELL),
            bins_x: bins[0],
            bins_y: bins[1],
            bins_z: bins[2],
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: pool, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: binned, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: ranges, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: order, offset: 0 },
                GpuBinding::Buffer { binding: 5, buffer: solid, offset: 0 },
                GpuBinding::Buffer { binding: 6, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.keep_whitewater",
        );
    }
}
