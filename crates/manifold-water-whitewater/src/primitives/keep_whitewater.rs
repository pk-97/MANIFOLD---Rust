//! `node.keep_whitewater` — FLIP's whitewater removal as a keep flag per pool
//! slot. A
//! per-element gather on the codegen path; `node.running_total` over the
//! flags and the stage's compaction pass finish the removal.
//!
//! Mesh outflows also port fluidsimulation.cpp::_updateOutflowMeshFluidSource.
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::freeze::classify::FusedOutputCapacity;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};
use manifold_water_liquid::fluid_particles::CellRange;
use manifold_water_liquid::tile_pool::tile_pool;
use manifold_water_liquid::whitewater::{WHITEWATER_COMMON, WhitewaterParticle};
use manifold_water_liquid::{float_param, int_param};

/// FLIP's `_maxDiffuseParticlesPerCell`.
pub(crate) const MAX_PER_CELL: f32 = 5000.0;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct KeepUniforms {
    cap: f32,
    region_count: f32,
    region_offset: f32,
    tick_seconds: f32,
    cell_size: f32,
    pool_tiles: f32,
    open_faces: i32,
    bounds_lo: [f32; 3],
    bounds_hi: [f32; 3],
    kill_height: f32,
    dispatch_count: u32,
    _pad0: u32,
}

const _: () = assert!(std::mem::size_of::<KeepUniforms>() == 64);

manifold_node_engine::primitive! {
    name: KeepWhitewater,
    type_id: "node.keep_whitewater",
    purpose: "Which whitewater pool slots survive the tick, FLIP's removal with every side colliding: 1 to keep, 0 to remove. A slot goes when it is empty (kind 3), its lifetime is at or below 0, its position is not finite, it lies inside the solid or an enabled outflow mesh (strictly negative posed SDF, the liquid removal rule), it has left the scene where the liquid's move removes water (beyond the plane of an Open face of the Bounds box bounds_lo..bounds_hi, or below Kill Height), or its cell already holds Max Per Cell kept particles earlier in the pool. One u32 per slot.",
    inputs: {
        pool: Array(WhitewaterParticle) required,
        binned: Array(WhitewaterParticle) required,
        cell_ranges: Array(CellRange) required,
        order: Array(u32) required,
        solid: Array(f32) required,
        regions: Array(LiquidBody) optional, shapes: Array(LiquidShape) optional, atlas: Array(u32) optional,
        tiles: Array(u32) required,
        region_count: ScalarF32 optional, region_offset: ScalarF32 optional, tick_seconds: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
        open_faces: ScalarF32 optional,
        bounds_lo_x: ScalarF32 optional, bounds_lo_y: ScalarF32 optional, bounds_lo_z: ScalarF32 optional,
        bounds_hi_x: ScalarF32 optional, bounds_hi_y: ScalarF32 optional, bounds_hi_z: ScalarF32 optional,
        kill_height: ScalarF32 optional,
    },
    outputs: {
        out: Array(u32),
    },
    params: [
        float_param!("cap", "Max Per Cell", MAX_PER_CELL, 1.0, 1.0e6),
        float_param!("region_count", "Regions", 0.0, 0.0, 16_777_216.0),
        float_param!("region_offset", "Region Offset", 0.0, 0.0, 16_777_216.0),
        float_param!("tick_seconds", "Pose Time", 0.0, 0.0, 1000.0),
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
        int_param!("open_faces", "Open Faces (bits −X +X −Y +Y −Z +Z)", 0.0, 0.0, 63.0),
        float_param!("bounds_lo_x", "Bounds Min X", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_lo_y", "Bounds Min Y", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_lo_z", "Bounds Min Z", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_hi_x", "Bounds Max X", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_hi_y", "Bounds Max Y", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_hi_z", "Bounds Max Z", 0.0, -1.0e4, 1.0e4),
        float_param!("kill_height", "Kill Height", f32::MIN, f32::MIN, f32::MAX),
    ],
    depth_rule: Terminal,
    composition_notes: "The removal step at the end of the GPU whitewater tick, after node.preserve_foam. Wire pool and binned from the same pool, and that pool into a pooled sort over the liquid step's world tiles with the step's cell size; cell_ranges (one per pooled element) and order come from that sort. tiles, pool_tiles and cell_size from the liquid step, solid the node array on the same tiles as node.advect_whitewater takes it.",
    examples: [],
    summary: "Decides which foam, spray and bubbles survive this step: the dead, the stray and the overcrowded go.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater removal", "remove whitewater", "diffuse particle removal"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/keep_whitewater_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "pool" },
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON, LIQUID_POSE, LIQUID_COLLIDER],
}

impl Primitive for KeepWhitewater {
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
        let grid = match tile_pool(ctx, "Keep Whitewater") {
            Ok(grid) => grid,
            Err(refusal) => {
                ctx.error(refusal);
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (
            Some(pool),
            Some(binned),
            Some(ranges),
            Some(order),
            Some(solid),
            Some(tiles),
            Some(out),
        ) = (
            ctx.inputs.array("pool"),
            ctx.inputs.array("binned"),
            ctx.inputs.array("cell_ranges"),
            ctx.inputs.array("order"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        )
        else {
            return;
        };
        let elements = grid.elements();
        if elements * 4 > solid.size || elements * size_of::<CellRange>() as u64 > ranges.size {
            let pool_tiles = grid.pool_tiles;
            ctx.error(format!(
                "Keep Whitewater: a pool of {pool_tiles} tiles is larger than its solid or cell ranges"
            ));
            return;
        }
        let count =
            (pool.size / std::mem::size_of::<WhitewaterParticle>() as u64).min(out.size / 4) as u32;
        if count == 0 {
            return;
        }
        let uniforms = KeepUniforms {
            cap: ctx.scalar_or_param("cap", MAX_PER_CELL),
            region_count: ctx.scalar_or_param("region_count", 0.0),
            region_offset: ctx.scalar_or_param("region_offset", 0.0),
            tick_seconds: ctx.scalar_or_param("tick_seconds", 0.0),
            cell_size: grid.cell_size,
            pool_tiles: grid.pool_tiles as f32,
            open_faces: ctx.scalar_or_param("open_faces", 0.0).round() as i32,
            bounds_lo: ["bounds_lo_x", "bounds_lo_y", "bounds_lo_z"]
                .map(|name| ctx.scalar_or_param(name, 0.0)),
            bounds_hi: ["bounds_hi_x", "bounds_hi_y", "bounds_hi_z"]
                .map(|name| ctx.scalar_or_param(name, 0.0)),
            kill_height: ctx.scalar_or_param("kill_height", f32::MIN),
            dispatch_count: count,
            _pad0: 0,
        };
        if uniforms.region_count > 0.0 {
            for name in ["regions", "shapes", "atlas"] {
                if ctx.inputs.array(name).is_none() {
                    ctx.error(format!("Keep Whitewater: outflows require {name}"));
                    return;
                }
            }
        }
        let extra =
            ["regions", "shapes", "atlas"].map(|name| ctx.inputs.array(name).unwrap_or(solid));
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
                    buffer: solid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: extra[0],
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 7,
                    buffer: extra[1],
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 8,
                    buffer: extra[2],
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 9,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 10,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.keep_whitewater",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
