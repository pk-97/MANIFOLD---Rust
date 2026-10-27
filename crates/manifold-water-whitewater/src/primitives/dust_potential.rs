//! FLIP dust-emission potential near eligible solid objects.
//! Ported from FLIP Fluids (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use super::whitewater_obstacle_source::WhitewaterSource;
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

/// FLIP turbulence normalization defaults; dust uses 0.75 times the minimum.
pub(crate) const MIN_TURBULENCE: f32 = 100.0;
pub(crate) const MAX_TURBULENCE: f32 = 200.0;
pub(crate) const DUST_ENABLED: f32 = 0.0;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DustUniforms {
    min_turbulence: f32,
    max_turbulence: f32,
    dust_enabled: f32,
    boundary_dust: f32,
    bounds_lo_z: f32,
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
}

manifold_node_engine::primitive! {
    name: DustPotential,
    type_id: "node.dust_potential",
    purpose: "FLIP dust potential: enabled markers zero to 2.5 cells from a dust-emitting solid use strength times normalized turbulence with 0.75 times Min Turbulence and Max Turbulence. Domain sources require Boundary Dust and a marker within 1.5 cells of the Bounds' low z face (bounds_lo_z). Solid and source are the node arrays of node.whitewater_obstacle_source, turbulence the step's cell array, all read through its world-tile table. Defaults disable dust and boundary dust.",
    inputs: {
        particles: Array(FluidParticle) required,
        solid: Array(f32) required,
        turbulence: Array(f32) required,
        source: Array(WhitewaterSource) required,
        tiles: Array(u32) required,
        min_turbulence: ScalarF32 optional,
        max_turbulence: ScalarF32 optional,
        dust_enabled: ScalarF32 optional, boundary_dust: ScalarF32 optional,
        bounds_lo_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("min_turbulence", "Min Turbulence", MIN_TURBULENCE, 0.0, 1.0e5),
        float_param!("max_turbulence", "Max Turbulence", MAX_TURBULENCE, 0.0, 1.0e5),
        float_param!("dust_enabled", "Dust Emission", DUST_ENABLED, 0.0, 1.0),
        float_param!("boundary_dust", "Boundary Dust", 0.0, 0.0, 1.0),
        float_param!("bounds_lo_z", "Bounds Min Z", 0.0, -1.0e4, 1.0e4),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16777216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Use unscaled sampled marker velocities and energy, then turbulence_emission_count with wavecrest rate zero and turbulence rate equal to dust rate. Type these spawns as dust after normal emission. Source uses nearest-object properties at the marker cell index.",
    examples: [],
    summary: "Finds turbulent dust emitters near solid objects.",
    category: Particles3D,
    role: Filter,
    aliases: ["dust potential", "sediment emission"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/dust_potential_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "particles" },
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON],
}

impl Primitive for DustPotential {
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
        let pool = match tile_pool(ctx, "Dust Potential") {
            Ok(pool) => pool,
            Err(refusal) => {
                ctx.error(refusal);
                return;
            }
        };
        let min_turbulence = ctx.scalar_or_param("min_turbulence", MIN_TURBULENCE);
        let max_turbulence = ctx.scalar_or_param("max_turbulence", MAX_TURBULENCE);
        if !(max_turbulence > min_turbulence
            && min_turbulence.is_finite()
            && max_turbulence.is_finite())
        {
            ctx.error(format!(
                "Dust Potential: Max Turbulence {max_turbulence} does not lie above Min Turbulence {min_turbulence}"
            ));
            return;
        }
        let dust_enabled = ctx.scalar_or_param("dust_enabled", DUST_ENABLED);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let (Some(solid), Some(turbulence), Some(source), Some(tiles)) = (
            ctx.inputs.array("solid"),
            ctx.inputs.array("turbulence"),
            ctx.inputs.array("source"),
            ctx.inputs.array("tiles"),
        ) else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let total = pool.elements();
        if total * 4 > solid.size || total * 4 > turbulence.size || total * 16 > source.size {
            ctx.error(format!(
                "Dust Potential: a pool of {} tiles is larger than its arrays",
                pool.pool_tiles
            ));
            return;
        }
        let count =
            (particles.size / std::mem::size_of::<FluidParticle>() as u64).min(out.size / 4) as u32;
        if count == 0 {
            return;
        }
        let uniforms = DustUniforms {
            min_turbulence,
            max_turbulence,
            dust_enabled,
            boundary_dust: ctx.scalar_or_param("boundary_dust", 0.0),
            bounds_lo_z: ctx.scalar_or_param("bounds_lo_z", 0.0),
            cell_size: pool.cell_size,
            pool_tiles: pool.pool_tiles as f32,
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
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: solid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: turbulence,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: source,
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
            "node.dust_potential",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
