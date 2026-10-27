//! `node.whitewater_type` — spray, foam or bubble for each new whitewater
//! particle, by FLIP's own rule. A per-element atom on the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;
use manifold_water_liquid::fluid_particles::WhitewaterSpawn;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::freeze::classify::FusedOutputCapacity;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_pool::tile_pool;
use manifold_water_liquid::whitewater::WHITEWATER_COMMON;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`,
/// padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TypeUniforms {
    spray_speed: f32,
    seed: f32,
    epoch: f32,
    dust: f32,
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
}

manifold_node_engine::primitive! {
    name: WhitewaterType,
    type_id: "node.whitewater_type",
    purpose: "Sort each new whitewater particle into bubble (0), foam (1) or spray (2) as FLIP types a fresh particle: spray for a particle whose cell lies in no stored tile of the liquid step's world tiles; else foam within one cell of the liquid surface (the distance read trilinearly at cell centres, a cell in an absent tile reading 3 cells of air), bubble deeper, spray higher; and foam or spray whose cell has no air cell among its 26 neighbours becomes bubble. Dust Source selects dust (4, keeping saved GPU empty kind 3); fresh spray velocity is scaled uniformly from 1 to Spray Emission Speed. Slots with lifetime 0 pass whole.",
    inputs: {
        spawns: Array(WhitewaterSpawn) required,
        distance: Array(f32) required,
        cells: Array(u32) required,
        tiles: Array(u32) required,
        spray_speed: ScalarF32 optional, seed: ScalarF32 optional, epoch: ScalarF32 optional, dust: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(WhitewaterSpawn),
    },
    params: [
        float_param!("spray_speed", "Spray Emission Speed", 1.0, 1.0, 100.0),
        float_param!("seed", "Seed", 0.0, 0.0, 1.0e9),
        float_param!("epoch", "Epoch", 0.0, 0.0, 1.0e9),
        float_param!("dust", "Dust Source", 0.0, 0.0, 1.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16777216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.spawn_whitewater, before node.whitewater_lifecycle's spawns. distance from node.crossing_distance and cells from node.liquid_cells, on the world tiles node.spawn_whitewater reads (tiles, pool_tiles and cell_size the same). The lifecycle retypes every particle by the same rule each tick, so FLIP's foam depth is fixed here, not a param.",
    examples: [],
    summary: "Decides whether each new whitewater particle is spray, foam or a bubble, from where it sits against the water surface.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater type", "foam spray bubble", "diffuse particle type"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/whitewater_type_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "spawns" },
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON],
}

impl Primitive for WhitewaterType {
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
                    .find(|(name, _)| *name == "spawns")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Whitewater Type") {
            Ok(pool) => pool,
            Err(refusal) => {
                ctx.error(refusal);
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(spawns), Some(distance), Some(cells), Some(tiles), Some(out)) = (
            ctx.inputs.array("spawns"),
            ctx.inputs.array("distance"),
            ctx.inputs.array("cells"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        if pool.elements() * 4 > distance.size.min(cells.size) {
            ctx.error(format!(
                "Whitewater Type: a pool of {} tiles is larger than its distance or cells",
                pool.pool_tiles
            ));
            return;
        }
        let record = std::mem::size_of::<WhitewaterSpawn>() as u64;
        let count = (spawns.size.min(out.size) / record) as u32;
        if count == 0 {
            return;
        }
        let uniforms = TypeUniforms {
            spray_speed: ctx.scalar_or_param("spray_speed", 1.0),
            seed: ctx.scalar_or_param("seed", 0.0),
            epoch: ctx.scalar_or_param("epoch", 0.0),
            dust: ctx.scalar_or_param("dust", 0.0),
            cell_size: pool.cell_size,
            pool_tiles: pool.pool_tiles as f32,
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
                    buffer: spawns,
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
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.whitewater_type",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
