//! `node.spawn_whitewater` — one new whitewater particle per spawn slot,
//! placed by FLIP's own emitter rule around the liquid particle that emits
//! it. A per-element atom
//! on the codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;
use manifold_water_liquid::fluid_particles::WhitewaterSpawn;

use super::whitewater_lifecycle::{DEFAULT_CAPACITY, MAX_CAPACITY};
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

/// FLIP's lifetime rule (`_minDiffuseParticleLifetime`,
/// `_maxDiffuseParticleLifetime`, `_lifetimeVariance`), seconds.
pub(crate) const MIN_LIFETIME: f32 = 0.0;
pub(crate) const MAX_LIFETIME: f32 = 7.0;
pub(crate) const LIFETIME_VARIANCE: f32 = 3.0;

/// More emitter slots than any particle array holds: unwired, the whole
/// offsets array counts.
const ALL_EMITTERS: f32 = 16_777_216.0;

const CAPACITY_PARAMS: [&str; 1] = ["capacity"];

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpawnUniforms {
    capacity: f32,
    emitters: f32,
    seed: f32,
    epoch: f32,
    min_lifetime: f32,
    max_lifetime: f32,
    lifetime_variance: f32,
    dt: f32,
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
}

manifold_node_engine::primitive! {
    name: SpawnWhitewater,
    type_id: "node.spawn_whitewater",
    purpose: "Place this frame's new whitewater particles, one per spawn slot, by FLIP's emitter rule. The running total's last entry is the frame's emission count; slot j takes emission j, or past Capacity an even subset (emission ⌊j · total / Capacity⌋). Its emitter is the liquid particle whose running total first passes it. The new particle lands at random in a cylinder about that particle's velocity, 8 of FLIP's marker radii wide and Duration seconds of its travel long. It is dropped in a cell outside every stored tile of the liquid step's world tiles, or within a quarter cell of a solid; its lifetime is Min Lifetime + energy × (Max − Min) ± Lifetime Variance, dropped at or below 0; its velocity is the liquid's at its position, from the step's pooled faces. Dropped and unused slots have lifetime 0. Kind is left 0 for node.whitewater_type.",
    inputs: {
        offsets: Array(u32) required,
        particles: Array(FluidParticle) required,
        energy: Array(f32) required,
        faces: Array(FaceSample) required,
        solid: Array(f32) required,
        tiles: Array(u32) required,
        emitters: ScalarF32 optional,
        seed: ScalarF32 optional,
        epoch: ScalarF32 optional,
        dt: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(WhitewaterSpawn),
    },
    params: [
        float_param!("capacity", "Capacity", DEFAULT_CAPACITY as f32, 1.0, MAX_CAPACITY as f32),
        float_param!("emitters", "Emitter Slots", ALL_EMITTERS, 0.0, ALL_EMITTERS),
        float_param!("seed", "Seed", 0.0, -1.0e9, 1.0e9),
        float_param!("epoch", "Epoch", 0.0, 0.0, 1.0e9),
        float_param!("min_lifetime", "Min Lifetime", MIN_LIFETIME, 0.0, 1.0e3),
        float_param!("max_lifetime", "Max Lifetime", MAX_LIFETIME, 0.0, 1.0e3),
        float_param!("lifetime_variance", "Lifetime Variance", LIFETIME_VARIANCE, 0.0, 1.0e3),
        float_param!("dt", "Duration (s)", 1.0 / 60.0, 0.0, 1.0e3),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16777216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "offsets from node.running_total over node.emission_count's counts, emitters the same count the running total ran over (the particle frame's count). particles from node.sample_faces_at_particles and energy from node.energy_potential, so each emitter is read as it was scored. faces, solid, tiles, pool_tiles and cell_size as the rest of the whitewater chain reads them (the liquid step's pooled faces and node arrays, and the domain's cell size); seed and epoch as node.jitter_particles takes them. Capacity is the lifecycle's. Feed node.whitewater_type, then node.whitewater_lifecycle's spawns.",
    examples: [],
    summary: "Places the new foam, spray and bubble particles around the breaking water that throws them off.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater spawn", "emit whitewater", "diffuse particle spawn"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/spawn_whitewater_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::ParamProduct { params: &CAPACITY_PARAMS, plus: 0 },
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON, LIQUID_FACES],
}

/// Spawn slots for a param set: the Capacity param as the fused kernel
/// counts it.
fn spawn_slots(params: &ParamValues) -> u32 {
    match params.get("capacity") {
        Some(ParamValue::Float(v)) => v.round().max(0.0) as u32,
        _ => DEFAULT_CAPACITY,
    }
}

impl Primitive for SpawnWhitewater {
    fn array_output_capacity(
        &self,
        port: &str,
        params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out").then(|| spawn_slots(params))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Spawn Whitewater") {
            Ok(pool) => pool,
            Err(refusal) => {
                ctx.error(refusal);
                return;
            }
        };
        let slots = spawn_slots(ctx.params);
        let emitters = ctx.scalar_or_param("emitters", ALL_EMITTERS);
        let seed = ctx.scalar_or_param("seed", 0.0);
        let epoch = ctx.scalar_or_param("epoch", 0.0);
        let min_lifetime = ctx.param_f32("min_lifetime", MIN_LIFETIME);
        let max_lifetime = ctx.param_f32("max_lifetime", MAX_LIFETIME);
        let lifetime_variance = ctx.param_f32("lifetime_variance", LIFETIME_VARIANCE);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (
            Some(offsets),
            Some(particles),
            Some(energy),
            Some(faces),
            Some(solid),
            Some(tiles),
            Some(out),
        ) = (
            ctx.inputs.array("offsets"),
            ctx.inputs.array("particles"),
            ctx.inputs.array("energy"),
            ctx.inputs.array("faces"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        )
        else {
            return;
        };
        if pool.elements() * std::mem::size_of::<FaceSample>() as u64 > faces.size
            || pool.elements() * 4 > solid.size
        {
            ctx.error(format!(
                "Spawn Whitewater: a pool of {} tiles is larger than its faces or solid",
                pool.pool_tiles
            ));
            return;
        }
        let count =
            u64::from(slots).min(out.size / std::mem::size_of::<WhitewaterSpawn>() as u64) as u32;
        if count == 0 || offsets.size == 0 || particles.size == 0 || energy.size == 0 {
            return;
        }
        let uniforms = SpawnUniforms {
            capacity: slots as f32,
            emitters,
            seed,
            epoch,
            min_lifetime,
            max_lifetime,
            lifetime_variance,
            dt: ctx.scalar_or_param("dt", 1.0 / 60.0),
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
                    buffer: offsets,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: energy,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: faces,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: solid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 7,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.spawn_whitewater",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
