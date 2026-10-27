//! `node.retype_whitewater` — FLIP's per-tick retype of every live
//! whitewater particle. A per-element atom on the codegen path.
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
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::grid::LIQUID_FACES;
use manifold_water_liquid::tile_pool::tile_pool;
use manifold_water_liquid::whitewater::{WHITEWATER_COMMON, WhitewaterParticle};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`,
/// padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RetypeUniforms {
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
}

manifold_node_engine::primitive! {
    name: RetypeWhitewater,
    type_id: "node.retype_whitewater",
    purpose: "Retypes each live whitewater particle after it moves, by FLIP's rule: foam within one cell of the liquid surface (the distance read trilinearly at cell centres), bubble deeper, spray higher; foam that would turn bubble stays foam until it is a further cell deeper; foam or spray whose cell has no air cell among its 26 neighbours becomes bubble (a neighbour in a tile the world-tile table does not hold is air). A bubble that turns foam or spray takes the liquid velocity at its position. Empty slots (kind 3) pass whole.",
    inputs: {
        pool: Array(WhitewaterParticle) required,
        distance: Array(f32) required,
        cells: Array(u32) required,
        faces: Array(FaceSample) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(WhitewaterParticle),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.advect_whitewater in the GPU whitewater tick, before node.age_whitewater. distance from node.crossing_distance and cells from node.liquid_cells, faces, tiles and pool_tiles from the liquid step, all on the same world tiles, cell_size the step's cell size. Uses the same rule as node.whitewater_type, plus FLIP's foam buffer, so new and old particles agree.",
    examples: [],
    summary: "Re-decides whether each whitewater particle is now spray, foam or a bubble after it has moved.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater retype", "foam to bubble", "diffuse particle types"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/retype_whitewater_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "pool" },
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON, LIQUID_FACES],
}

impl Primitive for RetypeWhitewater {
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
        let grid = match tile_pool(ctx, "Retype Whitewater") {
            Ok(grid) => grid,
            Err(refusal) => {
                ctx.error(refusal);
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(pool), Some(distance), Some(cells), Some(faces), Some(tiles), Some(out)) = (
            ctx.inputs.array("pool"),
            ctx.inputs.array("distance"),
            ctx.inputs.array("cells"),
            ctx.inputs.array("faces"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let elements = grid.elements();
        if elements * 4 > distance.size.min(cells.size)
            || elements * size_of::<FaceSample>() as u64 > faces.size
        {
            let pool_tiles = grid.pool_tiles;
            ctx.error(format!(
                "Retype Whitewater: a pool of {pool_tiles} tiles is larger than its distance, cells or faces"
            ));
            return;
        }
        let count =
            (pool.size.min(out.size) / std::mem::size_of::<WhitewaterParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let uniforms = RetypeUniforms {
            cell_size: grid.cell_size,
            pool_tiles: grid.pool_tiles as f32,
            dispatch_count: count,
            _pad0: 0,
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
                    buffer: distance,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: cells,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: faces,
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
            "node.retype_whitewater",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
