//! `node.sample_faces_at_particles` — liquid velocity at each particle from
//! the liquid step's pooled faces, by FLIP's MAC trilinear. A per-element atom
//! on the codegen path.
//!
//! Ported from FLIP Fluids macvelocityfield.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

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
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::grid::LIQUID_FACES;
use manifold_water_liquid::tile_pool::tile_pool;
use manifold_water_liquid::whitewater::WHITEWATER_COMMON;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`,
/// padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SampleUniforms {
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
}

manifold_node_engine::primitive! {
    name: SampleFacesAtParticles,
    type_id: "node.sample_faces_at_particles",
    purpose: "Sets each live liquid particle's velocity from the liquid step's faces, as FLIP's whitewater reads its velocity field: each component is the trilinear mix of the eight faces around the particle on that component's staggered stencil (face f of an axis is the pooled face record of world cell f, found through the step's world-tile table), a face in an absent tile reads the canonical record, and a particle whose cell lies in no stored tile gets 0. Position, radius and id pass through; slots with radius 0 pass whole.",
    inputs: {
        particles: Array(FluidParticle) required,
        faces: Array(FaceSample) required,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16777216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "After node.jitter_particles in the whitewater emitter chain. faces, tiles and pool_tiles from node.gpu_flip_step, cell_size the domain's. Feeds node.energy_potential, node.wavecrest_potential and node.emission_count.",
    examples: [],
    summary: "Reads the liquid's flow at each particle, so whitewater knows how fast and which way the water there is moving.",
    category: Particles3D,
    role: Filter,
    aliases: ["liquid velocity at particles", "sample velocity", "MAC trilinear"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/sample_faces_at_particles_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "particles" },
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON, LIQUID_FACES],
}

impl Primitive for SampleFacesAtParticles {
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
        let pool = match tile_pool(ctx, "Sample Faces at Particles") {
            Ok(pool) => pool,
            Err(refusal) => {
                ctx.error(refusal);
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(particles), Some(faces), Some(tiles), Some(out)) = (
            ctx.inputs.array("particles"),
            ctx.inputs.array("faces"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        if faces.size < pool.elements() * std::mem::size_of::<FaceSample>() as u64 {
            ctx.error(format!(
                "Sample Faces at Particles: faces holds fewer than the {} records of a pool of {} tiles",
                pool.elements(),
                pool.pool_tiles
            ));
            return;
        }
        let count =
            (particles.size.min(out.size) / std::mem::size_of::<FluidParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let uniforms = SampleUniforms {
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
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: faces,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.sample_faces_at_particles",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
