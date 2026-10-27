//! `node.preserve_foam` — FLIP's foam preservation: foam in crowded cells
//! lives longer. A
//! per-element gather on the codegen path, over the bins of a pooled sort of
//! the same pool on the liquid step's world tiles.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::freeze::classify::FusedOutputCapacity;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::float_param;
use manifold_water_liquid::fluid_particles::CellRange;
use manifold_water_liquid::tile_pool::tile_pool;
use manifold_water_liquid::whitewater::WhitewaterParticle;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PreserveUniforms {
    enabled: f32,
    dt: f32,
    rate: f32,
    min_density: f32,
    max_density: f32,
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
}

manifold_node_engine::primitive! {
    name: PreserveFoam,
    type_id: "node.preserve_foam",
    purpose: "FLIP's foam preservation: each foam particle, dead or alive, gains rate · clamp((n − min_density) / (max_density − min_density), 0, 1) · dt seconds of lifetime, n the foam particles (dead ones too) in its cell. Cells are the bins of the pooled sort that binned the pool, one per element of a pooled field on the liquid step's world tiles; a particle in a tile the table does not hold has no cell and gains nothing. Off by default, as in FLIP; other slots pass whole.",
    inputs: {
        pool: Array(WhitewaterParticle) required,
        binned: Array(WhitewaterParticle) required,
        cell_ranges: Array(CellRange) required,
        order: Array(u32) required,
        tiles: Array(u32) required,
        enabled: ScalarF32 optional,
        dt: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
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
        float_param!("cell_size", "Cell Size", 0.0625, 0.001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.age_whitewater in the GPU whitewater tick, before removal. Wire pool and binned from the same pool, and that pool into a pooled sort over the liquid step's world tiles with the step's cell size, so its bins are FLIP's cells; cell_ranges (one per pooled element) and order come from that sort, tiles, pool_tiles and cell_size from the step.",
    examples: [],
    summary: "Keeps foam alive longer where lots of it has gathered, so thick foam lingers.",
    category: Particles3D,
    role: Filter,
    aliases: ["foam preservation", "foam density", "whitewater lifetime"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/preserve_foam_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "pool" },
    wgsl_includes: [READ_SIDE],
}

impl Primitive for PreserveFoam {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "pool")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let grid = match tile_pool(ctx, "Preserve Foam") {
            Ok(grid) => grid,
            Err(refusal) => {
                ctx.error(refusal);
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(pool), Some(binned), Some(ranges), Some(order), Some(tiles), Some(out)) = (
            ctx.inputs.array("pool"),
            ctx.inputs.array("binned"),
            ctx.inputs.array("cell_ranges"),
            ctx.inputs.array("order"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        if grid.elements() * size_of::<CellRange>() as u64 > ranges.size {
            let pool_tiles = grid.pool_tiles;
            ctx.error(format!(
                "Preserve Foam: the cell ranges hold fewer than a pool of {pool_tiles} tiles' elements"
            ));
            return;
        }
        let count =
            (pool.size.min(out.size) / std::mem::size_of::<WhitewaterParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let uniforms = PreserveUniforms {
            enabled: ctx.scalar_or_param("enabled", 0.0),
            dt: ctx.scalar_or_param("dt", 1.0 / 60.0),
            rate: ctx.scalar_or_param("rate", 0.75),
            min_density: ctx.scalar_or_param("min_density", 20.0),
            max_density: ctx.scalar_or_param("max_density", 45.0),
            cell_size: grid.cell_size,
            pool_tiles: grid.pool_tiles as f32,
            dispatch_count: count,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: pool,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: binned,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: ranges,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: order,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.preserve_foam",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
