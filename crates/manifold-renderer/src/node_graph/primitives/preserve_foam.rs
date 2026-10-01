//! `node.preserve_foam` — FLIP's foam preservation: foam in crowded cells
//! lives longer (`docs/GPU_WHITEWATER_DESIGN.md` section 3.9, phase L3). A
//! per-element gather on the codegen path, over the bins of a
//! `node.sort_particles_into_cells` of the same pool.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::{bin_param, float_param, read_searched_bins};
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::CellRange;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::WhitewaterParticle;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PreserveUniforms {
    enabled: f32,
    dt: f32,
    rate: f32,
    min_density: f32,
    max_density: f32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    cell_size: f32,
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
    dispatch_count: u32,
}

crate::primitive! {
    name: PreserveFoam,
    type_id: "node.preserve_foam",
    purpose: "FLIP's foam preservation: each foam particle, dead or alive, gains rate · clamp((n − min_density) / (max_density − min_density), 0, 1) · dt seconds of lifetime, n the foam particles (dead ones too) in its cell. Cells are the bins of the sort that binned the pool. Off by default, as in FLIP; other slots pass whole.",
    inputs: {
        pool: Array(WhitewaterParticle) required,
        binned: Array(WhitewaterParticle) required,
        cell_ranges: Array(CellRange) required,
        order: Array(u32) required,
        enabled: ScalarF32 optional,
        dt: ScalarF32 optional,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        bins_x: ScalarF32 optional, bins_y: ScalarF32 optional, bins_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(WhitewaterParticle),
    },
    params: [
        float_param!("enabled", "Preserve Foam", 0.0, 0.0, 1.0),
        float_param!("dt", "Tick", 1.0 / 60.0, 0.0001, 1.0),
        float_param!("rate", "Preservation Rate", 0.75, 0.0, 100.0),
        float_param!("min_density", "Min Foam Density", 20.0, 0.0, 10_000.0),
        float_param!("max_density", "Max Foam Density", 45.0, 0.0, 10_000.0),
        float_param!("center_x", "Center X", 0.0, -1.0e4, 1.0e4),
        float_param!("center_y", "Center Y", 0.0, -1.0e4, 1.0e4),
        float_param!("center_z", "Center Z", 0.0, -1.0e4, 1.0e4),
        float_param!("size_x", "Size X", 4.0, 0.001, 1.0e4),
        float_param!("size_y", "Size Y", 4.0, 0.001, 1.0e4),
        float_param!("size_z", "Size Z", 4.0, 0.001, 1.0e4),
        float_param!("cell_size", "Cell Size", 0.0625, 0.001, 100.0),
        bin_param!("bins_x", "Bins X"),
        bin_param!("bins_y", "Bins Y"),
        bin_param!("bins_z", "Bins Z"),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.age_whitewater in the GPU whitewater tick, before removal. Wire pool and binned from the same pool, and that pool into a node.sort_particles_into_cells whose box is the whitewater grid and cell_size its cell, so its bins are FLIP's cells; cell_ranges, order and bins_x/y/z come from that sort, the box and cell size are the ones it was given.",
    examples: [],
    picker: { label: "Preserve Foam", category: Atom },
    summary: "Keeps foam alive longer where lots of it has gathered, so thick foam lingers.",
    category: Particles3D,
    role: Filter,
    aliases: ["foam preservation", "foam density", "whitewater lifetime"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/preserve_foam_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "pool" },
    wgsl_includes: [],
}

impl Primitive for PreserveFoam {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "pool").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(pool), Some(binned), Some(ranges), Some(order), Some(out)) = (
            ctx.inputs.array("pool"),
            ctx.inputs.array("binned"),
            ctx.inputs.array("cell_ranges"),
            ctx.inputs.array("order"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let count = (pool.size.min(out.size) / std::mem::size_of::<WhitewaterParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let bins = match read_searched_bins(ctx, ranges.size, "Preserve Foam") {
            Ok(Some(bins)) => bins.map(|n| n as i32),
            // No lattice yet: nothing is binned, so no foam gains anything.
            Ok(None) => [0; 3],
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let [center_x, center_y, center_z] = ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let [size_x, size_y, size_z] = ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let uniforms = PreserveUniforms {
            enabled: ctx.scalar_or_param("enabled", 0.0),
            dt: ctx.scalar_or_param("dt", 1.0 / 60.0),
            rate: ctx.scalar_or_param("rate", 0.75),
            min_density: ctx.scalar_or_param("min_density", 20.0),
            max_density: ctx.scalar_or_param("max_density", 45.0),
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            cell_size: ctx.scalar_or_param("cell_size", 0.0625),
            bins_x: bins[0],
            bins_y: bins[1],
            bins_z: bins[2],
            dispatch_count: count,
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
                GpuBinding::Buffer { binding: 5, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.preserve_foam",
        );
    }
}
