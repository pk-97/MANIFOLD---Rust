//! Ported from FLIP Fluids fluidsimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
//! `node.liquid_fill` — a liquid's starting particles: a pool on the floor
//! plus one box, one particle per half-cell site, at rest
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md section 1 (the step)). The sites are the FLIP Fluids
//! engine's seeding lattice, so both solvers start from the same water; a
//! site inside a collider or GPU FLIP's inset walls is left dead (radius 0),
//! as the engine seeds only where the solid distance is positive. A pure function of its params and
//! wires, so it owns its storage, sized to exactly the sites it covers, and
//! fills it once per change. A source atom on the codegen path.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer};

use crate::float_param;
use super::sort_particles_into_cells::{int_param};
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::particles::FluidParticle;
use manifold_core::fluid_domain::MAX_FLUID_ROLES;
use manifold_node_engine::ports::EXACT_F32_COUNT;
use crate::liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};
use crate::liquid::lattice::LiquidLattice;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

/// Sites per cell: two along each axis.
pub(super) const SITES_PER_CELL: u32 = 8;

/// Sites per axis of a lattice with `nodes` cells: two per cell.
fn site_lattice(nodes: [u32; 3]) -> [u32; 3] {
    nodes.map(|n| 2 * n)
}

/// The half-open site range [j0, j1) along one axis whose points
/// `min + (1/4 + j/2)·cell_size` lie in [lo, hi): the engine's rule for a box
/// of liquid (`_addNewFluidCellsAABB`, `AABB::isPointInside`). The domain
/// turns its metres into sites with it.
pub(super) fn site_range(lo: f64, hi: f64, min: f64, cell_size: f64, cells: u32) -> [u32; 2] {
    let first = |x: f64| (2.0 * (x - min) / cell_size - 0.5).ceil().clamp(0.0, f64::from(2 * cells)) as u32;
    [first(lo), first(hi).max(first(lo))]
}

/// The sites a pool plus a box clipped above it fill, in the kernel's
/// enumeration: `pool` site layers of the whole floor, then the box.
pub(crate) fn filled_sites(nodes: [u32; 3], pool: u32, sites: [[u32; 2]; 3]) -> u64 {
    let n = site_lattice(nodes);
    let pool = pool.min(n[1]);
    let clip = |d: usize, lo: u32| {
        let a = lo.min(n[d]);
        let b = sites[d][1].min(n[d]);
        u64::from(b.saturating_sub(a))
    };
    let pool_sites = u64::from(n[0]) * u64::from(pool) * u64::from(n[2]);
    pool_sites + clip(0, sites[0][0]) * clip(1, sites[1][0].max(pool)) * clip(2, sites[2][0])
}

/// The fill from `read` (`scalar_or_param`): pool height and box in sites.
pub(crate) fn fill_of(read: impl Fn(&str, f32) -> f32) -> (u32, [[u32; 2]; 3]) {
    let int = |name: &str, default: f32| read(name, default).round().max(0.0) as u32;
    let pool = int("pool_sites", 5.0);
    let sites = [
        [int("box_x0", 0.0), int("box_x1", 0.0)],
        [int("box_y0", 0.0), int("box_y1", 0.0)],
        [int("box_z0", 0.0), int("box_z1", 0.0)],
    ];
    (pool, sites)
}

/// Particles the params place: the storage the plan starts from. Wires that
/// move the fill resize it at run time.
fn planned_particles(params: &ParamValues) -> u32 {
    let read = |name: &str, default: f32| match params.get(name) {
        Some(ParamValue::Float(v)) => *v,
        _ => default,
    };
    let Ok(lattice) = LiquidLattice::from_scalars(read) else { return 1 };
    let (pool, sites) = fill_of(read);
    pool_slots(filled_sites(lattice.cells(), pool, sites), read("particle_capacity", 0.0)).clamp(1, u64::from(EXACT_F32_COUNT))
        as u32
}

/// Particle slots the pool holds: the fill, or `capacity` when larger (0
/// means the fill's count). Slots past the fill start dead; sources emit
/// into them.
pub(crate) fn pool_slots(placed: u64, capacity: f32) -> u64 {
    placed.max(capacity.round().max(0.0) as u64)
}

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FillUniforms {
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    pool_sites: i32,
    box_x0: i32,
    box_x1: i32,
    box_y0: i32,
    box_y1: i32,
    box_z0: i32,
    box_z1: i32,
    jitter: f32,
    seed: i32,
    body_count: i32,
    epoch: i32,
    particle_capacity: i32,
    wall_inset: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

manifold_node_engine::primitive! {
    name: LiquidFill,
    type_id: "node.liquid_fill",
    purpose: "Place a liquid's starting particles at rest, one per half-cell site of the authored box inside the padded lattice (node.gpu_flip_domain's lattice wires: the box starts 3 cells in and has nodes − 7 cells per axis; site j at (1/4 + j/2) cells along each axis, 2 sites per cell): every site below pool_sites, then the box of sites [box_x0, x1) × [max(box_y0, pool_sites), y1) × [box_z0, z1), in lattice order. Each particle moves up to jitter / 4 cells each way by a hash of seed; 0 keeps the exact lattice. The radius is the sphere of an eighth of a cell; ids count from 1. A site inside one of the first body_count bodies at its pose when the epoch starts (its distance lattice at or below zero) is left dead, radius 0. With wall_inset above 0 (node.gpu_flip_domain's mesh_wall_inset) the test is the FLIP engine's instead: the six walls, wall_inset cells in from a grid 1.5 cells outside the box, and the bodies, read together trilinearly from that grid's nodes. The storage holds the larger of the sites covered and particle_capacity (0 = the sites covered), and count is its slot count: slots past the fill start dead, for sources to emit into. A pool past the 16,777,216 particles a count carries exactly is refused.",
    inputs: {
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        pool_sites: ScalarF32 optional,
        box_x0: ScalarF32 optional, box_x1: ScalarF32 optional,
        box_y0: ScalarF32 optional, box_y1: ScalarF32 optional,
        box_z0: ScalarF32 optional, box_z1: ScalarF32 optional,
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        body_count: ScalarF32 optional,
        epoch: ScalarF32 optional,
        particle_capacity: ScalarF32 optional,
        wall_inset: ScalarF32 optional,
    },
    outputs: {
        particles: Array(FluidParticle),
        count: ScalarF32,
    },
    params: [
        float_param!("lattice_min_x", "Lattice Min X", -2.1875, -1.0e4, 1.0e4),
        float_param!("lattice_min_y", "Lattice Min Y", -0.1875, -1.0e4, 1.0e4),
        float_param!("lattice_min_z", "Lattice Min Z", -2.1875, -1.0e4, 1.0e4),
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        float_param!("nodes_x", "Nodes X", 71.0, 8.0, 1024.0),
        float_param!("nodes_y", "Nodes Y", 71.0, 8.0, 1024.0),
        float_param!("nodes_z", "Nodes Z", 71.0, 8.0, 1024.0),
        int_param!("pool_sites", "Pool Height (half cells)", 5.0, 0.0, 2048.0),
        int_param!("box_x0", "Box Min X (half cell)", 0.0, 0.0, 2048.0),
        int_param!("box_x1", "Box Max X (half cell)", 0.0, 0.0, 2048.0),
        int_param!("box_y0", "Box Min Y (half cell)", 0.0, 0.0, 2048.0),
        int_param!("box_y1", "Box Max Y (half cell)", 0.0, 0.0, 2048.0),
        int_param!("box_z0", "Box Min Z (half cell)", 0.0, 0.0, 2048.0),
        int_param!("box_z1", "Box Max Z (half cell)", 0.0, 0.0, 2048.0),
        float_param!("jitter", "Jitter", 0.0, 0.0, 1.0),
        int_param!("seed", "Seed", 0.0, 0.0, 16_777_215.0),
        int_param!("body_count", "Bodies", 0.0, 0.0, MAX_FLUID_ROLES as f32),
        int_param!("epoch", "Epoch", 0.0, 0.0, 16_777_215.0),
        int_param!("particle_capacity", "Particle Capacity", 0.0, 0.0, 16_777_216.0),
        float_param!("wall_inset", "Wall Inset (cells)", 0.0, 0.0, 8.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Feeds node.liquid_state's seed and, through count, every atom that takes a live particle count. The pool and box sites, the padded lattice, bodies, shapes, atlas, body_count and epoch come from the liquid's domain, so a Resolution change refills at the new size and a restart refills around the colliders' starting poses.",
    examples: [],
    picker: { label: "Liquid Fill", category: Atom },
    summary: "Places the liquid's starting particles: a pool on the floor plus one block of water.",
    category: Particles3D,
    role: Source,
    aliases: ["seed liquid", "initial water", "dam break fill", "pool"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/liquid_fill_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather],
    wgsl_includes: [LIQUID_POSE, LIQUID_COLLIDER],
    extra_fields: {
        buffer: Option<GpuBuffer> = None,
        filled: Option<([u32; 20], usize)> = None,
    },
}

impl Primitive for LiquidFill {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "particles"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "particles").then_some(self.buffer.as_ref()).flatten()
    }

    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "particles").then(|| planned_particles(params))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(lattice) = LiquidLattice::from_wires(ctx, "Liquid Fill") else {
            return;
        };
        let nodes = lattice.cells();
        let (min, cell_size) = (lattice.min(), lattice.cell_size());
        let int = |name: &str, default: f32| ctx.scalar_or_param(name, default).round().max(0.0) as u32;
        let jitter = ctx.scalar_or_param("jitter", 0.0);
        let wall_inset = ctx.scalar_or_param("wall_inset", 0.0);
        if !(0.0..=8.0).contains(&wall_inset) {
            ctx.error("Liquid Fill: wall_inset must be 0 to 8 cells".to_string());
            return;
        }
        if !(0.0..=1.0).contains(&jitter) {
            ctx.error("Liquid Fill: jitter must be 0 to 1".to_string());
            return;
        }
        let (pool, sites) = fill_of(|name, default| ctx.scalar_or_param(name, default));
        let seed = int("seed", 0.0);
        let body_count = int("body_count", 0.0).min(MAX_FLUID_ROLES as u32) as i32;
        // With colliders the sites depend on their pose at the epoch's start.
        let epoch = if body_count > 0 { int("epoch", 0.0) } else { 0 };
        let requested = ctx.scalar_or_param("particle_capacity", 0.0);
        if !requested.is_finite() || requested < 0.0 {
            ctx.error("Liquid Fill: particle_capacity must be 0 or a positive count".to_string());
            return;
        }
        let slots = pool_slots(filled_sites(nodes, pool, sites), requested);
        if slots > u64::from(EXACT_F32_COUNT) {
            ctx.outputs.set_scalar("count", ParamValue::Float(0.0));
            ctx.error(format!(
                "Liquid Fill: the pool holds {slots} particles, more than the {EXACT_F32_COUNT} a particle count carries exactly"
            ));
            return;
        }
        let capacity = (slots as u32).max(1);
        ctx.outputs.set_scalar("count", ParamValue::Float(slots as f32));
        let bytes = u64::from(capacity) * std::mem::size_of::<FluidParticle>() as u64;
        if self.buffer.as_ref().is_none_or(|b| b.size != bytes) {
            match ctx.gpu_encoder().device.try_create_buffer_shared(bytes) {
                Ok(buffer) => {
                    self.buffer = Some(buffer);
                    self.filled = None;
                }
                Err(error) => {
                    ctx.error(format!("Liquid Fill needs {bytes} bytes of GPU storage: {error}"));
                    return;
                }
            }
        }
        let colliders = (ctx.inputs.array("bodies"), ctx.inputs.array("shapes"), ctx.inputs.array("atlas"));
        let gpu = ctx.gpu_encoder();
        let buffer = self.buffer.as_ref().expect("fill storage prepared");
        // Without all three collider arrays no body is read; the particle
        // buffer fills their slots.
        let (bodies, shapes, atlas, body_count) = match colliders {
            (Some(bodies), Some(shapes), Some(atlas)) => {
                let rows = (bodies.size / std::mem::size_of::<LiquidBody>() as u64).min(i32::MAX as u64) as i32;
                (bodies, shapes, atlas, body_count.min(rows))
            }
            _ => (buffer, buffer, buffer, 0),
        };
        let key = [
            min[0].to_bits(), min[1].to_bits(), min[2].to_bits(), cell_size.to_bits(),
            nodes[0], nodes[1], nodes[2], pool,
            sites[0][0], sites[0][1], sites[1][0], sites[1][1], sites[2][0], sites[2][1],
            jitter.to_bits(), seed, body_count as u32, epoch, capacity, wall_inset.to_bits(),
        ];
        if self.filled == Some((key, buffer.identity_key())) {
            return;
        }
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let clamp = |v: u32| v.min(i32::MAX as u32) as i32;
        let uniforms = FillUniforms {
            lattice_min_x: min[0],
            lattice_min_y: min[1],
            lattice_min_z: min[2],
            cell_size,
            nodes_x: lattice.nodes()[0] as f32,
            nodes_y: lattice.nodes()[1] as f32,
            nodes_z: lattice.nodes()[2] as f32,
            pool_sites: clamp(pool),
            box_x0: clamp(sites[0][0]),
            box_x1: clamp(sites[0][1]),
            box_y0: clamp(sites[1][0]),
            box_y1: clamp(sites[1][1]),
            box_z0: clamp(sites[2][0]),
            box_z1: clamp(sites[2][1]),
            jitter,
            seed: clamp(seed),
            body_count,
            epoch: clamp(epoch),
            particle_capacity: clamp(capacity),
            wall_inset,
            dispatch_count: capacity,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: bodies, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: shapes, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: atlas, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer, offset: 0 },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.liquid_fill",
        );
        self.filled = Some((key, buffer.identity_key()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquid_fill_counts_pool_plus_clipped_box() {
        // Pool 5 site layers of a 64³ floor (128 × 128 sites), box x 5..43,
        // y 0..67 clipped to 5..67, z 8..120.
        assert_eq!(filled_sites([64; 3], 5, [[5, 43], [0, 67], [8, 120]]), 128 * 5 * 128 + 38 * 62 * 112);
        // An empty box adds nothing; the pool never passes the lattice top.
        assert_eq!(filled_sites([8; 3], 40, [[0, 0], [0, 0], [0, 0]]), 16 * 16 * 16);
        // A box past the lattice is clipped to it.
        assert_eq!(filled_sites([8; 3], 0, [[12, 40], [0, 16], [0, 16]]), 4 * 16 * 16);
    }

    #[test]
    fn liquid_fill_site_range_is_the_engine_box_rule() {
        // Site j sits at (1/4 + j/2) cells; [lo, hi) keeps the sites inside.
        // 0.16 m on a 1/16 m cell is 2.56 cells: sites 0..=4 (at 0.25 to 2.25).
        assert_eq!(site_range(0.0, 0.16, 0.0, 0.0625, 64), [0, 5]);
        // A bound exactly on a site includes it at lo and excludes it at hi.
        assert_eq!(site_range(0.25, 1.25, 0.0, 1.0, 8), [0, 2]);
        // Clipped to the lattice, never inverted.
        assert_eq!(site_range(-5.0, 50.0, 0.0, 1.0, 8), [0, 16]);
        assert_eq!(site_range(3.0, 1.0, 0.0, 1.0, 8), [6, 6]);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
