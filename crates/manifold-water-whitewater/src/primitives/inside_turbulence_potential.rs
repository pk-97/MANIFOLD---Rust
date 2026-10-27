//! Normalized FLIP turbulence potential for markers outside the surface-emitter set.
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
use manifold_water_liquid::whitewater::{WHITEWATER_COMMON, whitewater_pool};

/// FLIP turbulence normalization defaults; dust uses 0.75 times the minimum.
pub(crate) const MIN_TURBULENCE: f32 = 100.0;
pub(crate) const MAX_TURBULENCE: f32 = 200.0;
pub(crate) const INSIDE_ENABLED: f32 = 1.0;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InsideUniforms {
    min_turbulence: f32,
    max_turbulence: f32,
    inside_enabled: f32,
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

manifold_node_engine::primitive! {
    name: InsideTurbulencePotential,
    type_id: "node.inside_turbulence_potential",
    purpose: "Sample FLIP turbulence at p minus half a cell through the liquid step's world-tile table (a cell in an absent tile reads the canonical element), clamp to Min/Max Turbulence and normalize. Exclude surface particles (within 1.5 cells of the surface and bordering air). Inside Enabled zero suppresses this source.",
    inputs: {
        particles: Array(FluidParticle) required,
        distance: Array(f32) required,
        turbulence: Array(f32) required,
        cells: Array(u32) required,
        tiles: Array(u32) required,
        min_turbulence: ScalarF32 optional,
        max_turbulence: ScalarF32 optional,
        inside_enabled: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("min_turbulence", "Min Turbulence", MIN_TURBULENCE, 0.0, 1.0e5),
        float_param!("max_turbulence", "Max Turbulence", MAX_TURBULENCE, 0.0, 1.0e5),
        float_param!("inside_enabled", "Inside Enabled", INSIDE_ENABLED, 0.0, 1.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16777216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Sample turbulence_field with the same distance and shrunken material cells as wavecrest_potential, with the liquid step's tiles and pool_tiles and the domain's cell_size; feed turbulence_emission_count alongside the mutually exclusive wavecrest potential.",
    examples: [],
    summary: "Finds submerged turbulent emitters, including near-surface particles classified inside by FLIP.",
    category: Particles3D,
    role: Filter,
    aliases: ["inside emission", "turbulence potential", "bubble source"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/inside_turbulence_potential_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "particles" },
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON],
}

impl Primitive for InsideTurbulencePotential {
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
        let pool = match whitewater_pool(ctx, "Inside Turbulence Potential") {
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
                "Inside Turbulence Potential: Max Turbulence {max_turbulence} does not lie above Min Turbulence {min_turbulence}"
            ));
            return;
        }
        let inside_enabled = ctx.scalar_or_param("inside_enabled", INSIDE_ENABLED);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let (Some(distance), Some(turbulence), Some(cells), Some(tiles)) = (
            ctx.inputs.array("distance"),
            ctx.inputs.array("turbulence"),
            ctx.inputs.array("cells"),
            ctx.inputs.array("tiles"),
        ) else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let total = pool.elements();
        if total * 4 > distance.size || total * 4 > turbulence.size || total * 4 > cells.size {
            ctx.error(format!(
                "Inside Turbulence Potential: a pool of {} tiles is larger than its arrays",
                pool.pool_tiles
            ));
            return;
        }
        let count =
            (particles.size / std::mem::size_of::<FluidParticle>() as u64).min(out.size / 4) as u32;
        if count == 0 {
            return;
        }
        let uniforms = InsideUniforms {
            min_turbulence,
            max_turbulence,
            inside_enabled,
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
                    buffer: turbulence,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: cells,
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
            "node.inside_turbulence_potential",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
