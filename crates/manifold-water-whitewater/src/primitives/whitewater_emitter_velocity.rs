//! FLIP surface-emitter velocity boost before energy and wavecrest evaluation.
//! Ported from FLIP Fluids (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::freeze::classify::FusedOutputCapacity;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::READ_SIDE;
use manifold_water_liquid::float_param;
use manifold_water_liquid::tile_pool::tile_pool;
use manifold_water_liquid::whitewater::WHITEWATER_COMMON;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VelocityUniforms {
    spray_speed: f32,
    seed: f32,
    epoch: f32,
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

manifold_node_engine::primitive! {
    name: WhitewaterEmitterVelocity,
    type_id: "node.whitewater_emitter_velocity",
    purpose: "For surface-classified markers above minus 0.75 cell depth, scale sampled velocity uniformly between 1 and Spray Emission Speed before energy and wavecrest evaluation. Other particles pass unchanged. FLIP diffuseparticlesimulation.cpp:1699.",
    inputs: {
        particles: Array(FluidParticle) required,
        distance: Array(f32) required,
        cells: Array(u32) required,
        tiles: Array(u32) required,
        spray_speed: ScalarF32 optional,
        seed: ScalarF32 optional,
        epoch: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
    },
    params: [
        float_param!("spray_speed", "Spray Emission Speed", 1.0, 1.0, 100.0),
        float_param!("seed", "Seed", 0.0, 0.0, 1.0e9),
        float_param!("epoch", "Epoch", 0.0, 0.0, 1.0e9),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16777216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "After sample_faces_at_particles, before energy_potential and wavecrest_potential. distance and cells are the liquid step's pooled fields, with its tiles and pool_tiles, and cell_size the domain's. Keep this separate from fresh spray velocity scaling: FLIP draws a second random factor after spawn classification.",
    examples: [],
    summary: "Scales surface emitter velocity by the FLIP spray emission factor.",
    category: Particles3D,
    role: Filter,
    aliases: ["spray emitter velocity"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/whitewater_emitter_velocity_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "particles" },
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON],
}

impl Primitive for WhitewaterEmitterVelocity {
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
                    .find(|(name, _)| *name == "particles")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Whitewater Emitter Velocity") {
            Ok(pool) => pool,
            Err(refusal) => {
                ctx.error(refusal);
                return;
            }
        };
        let spray_speed = ctx.scalar_or_param("spray_speed", 1.0);
        let seed = ctx.scalar_or_param("seed", 0.0);
        let epoch = ctx.scalar_or_param("epoch", 0.0);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let (Some(distance), Some(cells), Some(tiles)) = (
            ctx.inputs.array("distance"),
            ctx.inputs.array("cells"),
            ctx.inputs.array("tiles"),
        ) else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        if pool.elements() * 4 > distance.size || pool.elements() * 4 > cells.size {
            ctx.error(format!(
                "Whitewater Emitter Velocity: a pool of {} tiles is larger than its arrays",
                pool.pool_tiles
            ));
            return;
        }
        let count = (particles.size / std::mem::size_of::<FluidParticle>() as u64)
            .min(out.size / std::mem::size_of::<FluidParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let uniforms = VelocityUniforms {
            spray_speed,
            seed,
            epoch,
            cell_size: pool.cell_size,
            pool_tiles: pool.pool_tiles as f32,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
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
                    buffer: particles,
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
            "node.whitewater_emitter_velocity",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
