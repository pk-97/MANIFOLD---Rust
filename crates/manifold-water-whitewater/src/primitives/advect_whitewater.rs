//! `node.advect_whitewater` — one FLIP tick of every live whitewater
//! particle by its type, with FLIP's collision march against the solid
//!A per-element
//! atom on the codegen path.
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
use manifold_nodes_physics::force_lattice::{FORCE_LATTICE, ForceLatticeBinding};
use manifold_water_liquid::float_param;
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::grid::LIQUID_FACES;
use manifold_water_liquid::tile_pool::tile_pool;
use manifold_water_liquid::whitewater::{WHITEWATER_COMMON, WhitewaterParticle};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`,
/// padded to 16 bytes; 28 words.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AdvectUniforms {
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    dt: f32,
    foam_advection: f32,
    bubble_buoyancy: f32,
    bubble_drag: f32,
    spray_drag: f32,
    spray_drag_variance: f32,
    spray_restitution: f32,
    spray_friction: f32,
    substep_count: f32,
    field_nodes_x: f32,
    field_nodes_y: f32,
    field_nodes_z: f32,
    field_spacing: f32,
    force_lattices: f32,
    tick_index: f32,
    first_tick: f32,
    cell_size: f32,
    pool_tiles: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

const _: () = assert!(std::mem::size_of::<AdvectUniforms>() == 112);

/// Motion defaults; the fused lifecycle kernel reads the same values.
pub(crate) const FOAM_ADVECTION: f32 = 1.0;
pub(crate) const BUBBLE_BUOYANCY: f32 = 4.0;
pub(crate) const BUBBLE_DRAG: f32 = 1.0;
pub(crate) const SPRAY_DRAG: f32 = 0.0;
pub(crate) const SPRAY_DRAG_VARIANCE: f32 = 0.25;
pub(crate) const SPRAY_RESTITUTION: f32 = 0.2;
pub(crate) const SPRAY_FRICTION: f32 = 0.0;

manifold_node_engine::primitive! {
    name: AdvectWhitewater,
    type_id: "node.advect_whitewater",
    purpose: "Moves each live whitewater particle over the accepted liquid substeps and their MAC velocities when a schedule is wired, otherwise one supplied duration. Scene acceleration and timestamped hits use the liquid field inputs. By type: spray falls under gravity with per-id drag and bounces off solids (restitution on the normal part, friction on the tangent part); bubbles rise against gravity and drag toward the liquid velocity; foam rides the liquid velocity. The liquid velocity is FLIP's MAC trilinear of the step's pooled faces (world tiles) at the old position, 0 where a face has no weight or its tile is not held. Every type then marches its path in half-cell steps and stops a quarter cell clear of the solid. A particle that ends up moving faster than 1.1 times its new speed, or whose travel is not finite, dies (lifetime -1e6). Empty slots (kind 3) pass whole.",
    inputs: {
        pool: Array(WhitewaterParticle) required,
        faces: Array(FaceSample) required,
        solid: Array(f32) required,
        substep_schedule: Array(f32) optional,
        substep_faces: Array(f32) optional,
        forces: Array(f32) optional, impulses: Array(f32) optional,
        tiles: Array(u32) required,
        substep_count: ScalarF32 optional,
        field_nodes_x: ScalarF32 optional,
        field_nodes_y: ScalarF32 optional,
        field_nodes_z: ScalarF32 optional,
        field_spacing: ScalarF32 optional,
        force_lattices: ScalarF32 optional,
        tick_index: ScalarF32 optional,
        first_tick: ScalarF32 optional,

        bounds_lo_x: ScalarF32 optional, bounds_lo_y: ScalarF32 optional, bounds_lo_z: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity_y: ScalarF32 optional, gravity_z: ScalarF32 optional,
        dt: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        out: Array(WhitewaterParticle),
    },
    params: [
        float_param!("bounds_lo_x", "Bounds Min X", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_lo_y", "Bounds Min Y", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_lo_z", "Bounds Min Z", 0.0, -1.0e4, 1.0e4),
        float_param!("gravity_x", "Gravity X", 0.0, -1000.0, 1000.0),
        float_param!("gravity_y", "Gravity Y", -9.81, -1000.0, 1000.0),
        float_param!("gravity_z", "Gravity Z", 0.0, -1000.0, 1000.0),
        float_param!("dt", "Tick", 1.0 / 60.0, 0.0001, 1.0),
        float_param!("foam_advection", "Foam Advection", FOAM_ADVECTION, 0.0, 1.0),
        float_param!("bubble_buoyancy", "Bubble Buoyancy", BUBBLE_BUOYANCY, 0.0, 100.0),
        float_param!("bubble_drag", "Bubble Drag", BUBBLE_DRAG, 0.0, 1.0),
        float_param!("spray_drag", "Spray Drag", SPRAY_DRAG, 0.0, 100.0),
        float_param!("spray_drag_variance", "Spray Drag Variance", SPRAY_DRAG_VARIANCE, 0.0, 1.0),
        float_param!("spray_restitution", "Spray Restitution", SPRAY_RESTITUTION, 0.0, 1.0),
        float_param!("spray_friction", "Spray Friction", SPRAY_FRICTION, 0.0, 1.0),
        float_param!("substep_count", "substep_count", 0.0, 0.0, 16_777_216.0),
        float_param!("field_nodes_x", "field_nodes_x", 2.0, 0.0, 16_777_216.0),
        float_param!("field_nodes_y", "field_nodes_y", 2.0, 0.0, 16_777_216.0),
        float_param!("field_nodes_z", "field_nodes_z", 2.0, 0.0, 16_777_216.0),
        float_param!("field_spacing", "field_spacing", 0.25, 0.0, 16_777_216.0),
        float_param!("force_lattices", "force_lattices", 0.0, 0.0, 16_777_216.0),
        float_param!("tick_index", "tick_index", 0.0, 0.0, 16_777_216.0),
        float_param!("first_tick", "first_tick", 0.0, 0.0, 16_777_216.0),
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The first step of the GPU whitewater tick, on the pool node.array_feedback carries; retyping, lifetimes and removal follow. faces, tiles and pool_tiles from the liquid step's pooled faces and world tiles, cell_size its cell size, solid the node array on the same tiles (the value at each cell's low corner), bounds_lo_x/y/z the domain's, where the force lattices start. substep_schedule and substep_faces from the step's history, which lies on the table the step publishes as tiles. FLIP's ballistic and kill limit behaviours are not ported: every side collides, and a wall is a solid.",
    examples: [],
    summary: "Moves foam, spray and bubbles one step: spray flies and bounces, bubbles rise, foam rides the water.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater advection", "move whitewater", "diffuse particle advection"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/advect_whitewater_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "pool" },
    wgsl_includes: [READ_SIDE, WHITEWATER_COMMON, LIQUID_FACES, FORCE_LATTICE],
}

impl Primitive for AdvectWhitewater {
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
        let grid = match tile_pool(ctx, "Advect Whitewater") {
            Ok(grid) => grid,
            Err(refusal) => {
                ctx.error(refusal);
                return;
            }
        };
        let bounds_lo = ["bounds_lo_x", "bounds_lo_y", "bounds_lo_z"]
            .map(|name| ctx.scalar_or_param(name, 0.0));
        let gravity = [("gravity_x", 0.0), ("gravity_y", -9.81), ("gravity_z", 0.0)]
            .map(|(name, default)| ctx.scalar_or_param(name, default));
        let dt = ctx.scalar_or_param("dt", 1.0 / 60.0);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(pool), Some(faces), Some(solid), Some(tiles), Some(out)) = (
            ctx.inputs.array("pool"),
            ctx.inputs.array("faces"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("tiles"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        let elements = grid.elements();
        if faces.size < elements * size_of::<FaceSample>() as u64 || solid.size < elements * 4 {
            let pool_tiles = grid.pool_tiles;
            ctx.error(format!(
                "Advect Whitewater: faces or solid holds fewer than the {elements} elements of a pool of {pool_tiles} tiles"
            ));
            return;
        }
        let count =
            (pool.size.min(out.size) / std::mem::size_of::<WhitewaterParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let [bounds_lo_x, bounds_lo_y, bounds_lo_z] = bounds_lo;
        let [gravity_x, gravity_y, gravity_z] = gravity;
        let field = match ForceLatticeBinding::read(
            ctx,
            ctx.inputs.array("forces"),
            ctx.inputs.array("impulses"),
            "Advect Whitewater",
        ) {
            Ok(field) => field,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let uniforms = AdvectUniforms {
            bounds_lo_x,
            bounds_lo_y,
            bounds_lo_z,
            gravity_x,
            gravity_y,
            gravity_z,
            dt,
            foam_advection: ctx.scalar_or_param("foam_advection", 1.0),
            bubble_buoyancy: ctx.scalar_or_param("bubble_buoyancy", 4.0),
            bubble_drag: ctx.scalar_or_param("bubble_drag", 1.0),
            spray_drag: ctx.scalar_or_param("spray_drag", 0.0),
            spray_drag_variance: ctx.scalar_or_param("spray_drag_variance", 0.25),
            spray_restitution: ctx.scalar_or_param("spray_restitution", 0.2),
            spray_friction: ctx.scalar_or_param("spray_friction", 0.0),
            substep_count: ctx.scalar_or_param("substep_count", 0.0),
            field_nodes_x: field.nodes[0] as f32,
            field_nodes_y: field.nodes[1] as f32,
            field_nodes_z: field.nodes[2] as f32,
            field_spacing: field.spacing,
            force_lattices: field.force_lattices as f32,
            tick_index: ctx.scalar_or_param("tick_index", 0.0),
            first_tick: field.first_tick as f32,
            cell_size: grid.cell_size,
            pool_tiles: grid.pool_tiles as f32,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let steps = uniforms.substep_count as u64;
        if steps > 0 {
            for (name, bytes) in [
                ("substep_schedule", steps * 16),
                ("substep_faces", steps * elements * 16),
            ] {
                if ctx.inputs.array(name).is_none_or(|b| b.size < bytes) {
                    ctx.error(format!(
                        "Advect Whitewater: {name} does not cover {steps} accepted substep slots"
                    ));
                    return;
                }
            }
        }
        let extra = ["substep_schedule", "substep_faces", "forces", "impulses"]
            .map(|name| ctx.inputs.array(name).unwrap_or(solid));
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
                    buffer: faces,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: solid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: extra[0],
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: extra[1],
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: extra[2],
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 7,
                    buffer: extra[3],
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 8,
                    buffer: tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 9,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.advect_whitewater",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
