//! `node.wavecrest_potential` — FLIP's wavecrest potential per liquid
//! particle on the whitewater grid's fields. A per-element atom on the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp and interpolation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

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
use manifold_water_liquid::whitewater::{KnownValue, WHITEWATER_COMMON, whitewater_pool};

/// FLIP's defaults: curvature × cell size from which a crest starts to
/// emit, where it emits fully, and the least cosine between the particle's
/// direction and the surface normal.
pub(crate) const MIN_CURVATURE: f32 = 0.4;
pub(crate) const MAX_CURVATURE: f32 = 1.0;
pub(crate) const SHARPNESS: f32 = 0.4;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WavecrestUniforms {
    min_curvature: f32,
    max_curvature: f32,
    sharpness: f32,
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

manifold_node_engine::primitive! {
    name: WavecrestPotential,
    type_id: "node.wavecrest_potential",
    purpose: "How strongly a liquid particle sits on a breaking crest, as FLIP's whitewater measures it, 0 to 1. Only particles within 1.5 cells of the surface whose cell touches air (any of 26 neighbours) count. The surface's curvature there, times the cell size, must reach Min Curvature (it counts fully at Max Curvature), and the particle must move out through the surface: the cosine between its direction and the surface normal at least Sharpness. Distance and curvature are read trilinearly at cell centres, through the liquid step's world-tile table; a cell in an absent tile reads the canonical element. One f32 per particle slot; 0 for slots with radius 0.",
    inputs: {
        particles: Array(FluidParticle) required,
        distance: Array(f32) required,
        curvature: Array(KnownValue) required,
        cells: Array(u32) required,
        tiles: Array(u32) required,
        min_curvature: ScalarF32 optional,
        max_curvature: ScalarF32 optional,
        sharpness: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("min_curvature", "Min Curvature", MIN_CURVATURE, 0.0, 10.0),
        float_param!("max_curvature", "Max Curvature", MAX_CURVATURE, 0.0, 10.0),
        float_param!("sharpness", "Sharpness", SHARPNESS, -1.0, 1.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16777216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.sample_faces_at_particles in the whitewater emitter chain. distance from node.crossing_distance, curvature from the last node.extend_lattice, cells from node.liquid_cells, all pooled on the liquid step's world tiles with its tiles and pool_tiles, and cell_size the domain's. Feeds node.emission_count.",
    examples: [],
    summary: "Scores how sharply each bit of water is breaking over a wave crest, which is where foam and spray come from.",
    category: Particles3D,
    role: Filter,
    aliases: ["wave crest", "crest score", "breaking wave"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/wavecrest_potential_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "particles" },
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON],
}

impl Primitive for WavecrestPotential {
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
        let pool = match whitewater_pool(ctx, "Wavecrest Potential") {
            Ok(pool) => pool,
            Err(refusal) => {
                ctx.error(refusal);
                return;
            }
        };
        let min_curvature = ctx.scalar_or_param("min_curvature", MIN_CURVATURE);
        let max_curvature = ctx.scalar_or_param("max_curvature", MAX_CURVATURE);
        if !(max_curvature > min_curvature
            && min_curvature.is_finite()
            && max_curvature.is_finite())
        {
            ctx.error(format!(
                "Wavecrest Potential: Max Curvature {max_curvature} does not lie above Min Curvature {min_curvature}"
            ));
            return;
        }
        let sharpness = ctx.scalar_or_param("sharpness", SHARPNESS);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let (Some(distance), Some(curvature), Some(cells), Some(tiles)) = (
            ctx.inputs.array("distance"),
            ctx.inputs.array("curvature"),
            ctx.inputs.array("cells"),
            ctx.inputs.array("tiles"),
        ) else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let total = pool.elements();
        if total * 4 > distance.size || total * 8 > curvature.size || total * 4 > cells.size {
            ctx.error(format!(
                "Wavecrest Potential: a pool of {} tiles is larger than its arrays",
                pool.pool_tiles
            ));
            return;
        }
        let count =
            (particles.size / std::mem::size_of::<FluidParticle>() as u64).min(out.size / 4) as u32;
        if count == 0 {
            return;
        }
        let uniforms = WavecrestUniforms {
            min_curvature,
            max_curvature,
            sharpness,
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
                    buffer: curvature,
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
            "node.wavecrest_potential",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
