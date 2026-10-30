//! Ported from FLIP Fluids fluidsimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
//! `node.liquid_fill` — a liquid's starting particles: a pool on the floor
//! plus one box, one particle per half-cell site, at rest
//! The sites are the FLIP Fluids
//! engine's seeding lattice, so both solvers start from the same water. The
//! domain puts its box on whole world cells, so a site is at the same world
//! position (half cell J at (J/2 - 1/4)·h) whatever the box's corner; a
//! site inside a collider or beyond a Wall face of the Bounds box is left
//! dead (radius 0), as the engine seeds only where the solid distance is positive. A pure function of its params and
//! wires, so it fills once per change: every site into scratch, then only
//! the live ones, in site order, into storage it owns. Their count comes back
//! from the GPU at once, so a shaped fill holds only the water it places, not
//! its whole box.

use manifold_gpu::BufferUse;
use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::ports::EXACT_F32_COUNT;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_water_liquid::bodies::{
    COLLIDER_BVH, LIQUID_COLLIDER, LIQUID_FILL_SHAPE, LIQUID_POSE, LiquidBody, LiquidShape,
};
use manifold_water_liquid::primitives::prefix_scan::{PrefixScan, ScanLabels};
use manifold_water_liquid::primitives::sort_particles_into_cells::int_param;
use manifold_water_liquid::{PARTICLE_WATER, float_param};

const COMPACT_SHADER: &str = include_str!("shaders/liquid_fill_compact.wgsl");
const COMPACT_LABELS: ScanLabels = ScanLabels {
    blocks: "node.liquid_fill.scan_blocks",
    add: "node.liquid_fill.scan_add",
};

/// Sites per cell: two along each axis.
pub(super) const SITES_PER_CELL: u32 = 8;

/// Sites per axis of a box of `cells`: two per cell.
fn site_lattice(cells: [u32; 3]) -> [u32; 3] {
    cells.map(|n| 2 * n)
}

/// The box the fill's sites count from, from `read` (`scalar_or_param`):
/// its first corner in metres (`fill_min_x/y/z`), its cells per axis
/// (`fill_cells_x/y/z`) and the cell size. Defaults are the 4 m Dam Break
/// box at 0.0625 m. A count that is not whole and at least 1, a corner that
/// is not finite or a cell size that is not positive is refused by name.
pub(crate) fn fill_frame(
    read: impl Fn(&str, f32) -> f32,
) -> Result<([f32; 3], [u32; 3], f32), String> {
    let cells = |name: &str| {
        let value = read(name, 64.0);
        if value.fract() == 0.0 && (1.0..=EXACT_F32_COUNT as f32).contains(&value) {
            Ok(value as u32)
        } else {
            Err(format!(
                "{name} is {value}; it must be a whole cell count from 1 to {EXACT_F32_COUNT}"
            ))
        }
    };
    let corner = |name: &str, default: f32| {
        let value = read(name, default);
        value
            .is_finite()
            .then_some(value)
            .ok_or_else(|| format!("{name} is {value}; it must be finite"))
    };
    let cell_size = read("cell_size", 0.0625);
    if !(cell_size.is_finite() && cell_size > 0.0) {
        return Err(format!("cell_size is {cell_size}; it must be positive"));
    }
    Ok((
        [
            corner("fill_min_x", -2.0)?,
            corner("fill_min_y", 0.0)?,
            corner("fill_min_z", -2.0)?,
        ],
        [
            cells("fill_cells_x")?,
            cells("fill_cells_y")?,
            cells("fill_cells_z")?,
        ],
        cell_size,
    ))
}

/// The half-open site range [j0, j1) along one axis whose points
/// `min + (1/4 + j/2)·cell_size` lie in [lo, hi): the engine's rule for a box
/// of liquid (`_addNewFluidCellsAABB`, `AABB::isPointInside`). The domain
/// turns its metres into sites with it.
pub(super) fn site_range(lo: f64, hi: f64, min: f64, cell_size: f64, cells: u32) -> [u32; 2] {
    let first = |x: f64| {
        (2.0 * (x - min) / cell_size - 0.5)
            .ceil()
            .clamp(0.0, f64::from(2 * cells)) as u32
    };
    [first(lo), first(hi).max(first(lo))]
}

/// The pool's sites, the first the kernel enumerates: `pool` site layers of
/// the whole floor, never past the lattice top.
pub(crate) fn pool_count(cells: [u32; 3], pool: u32) -> u64 {
    let n = site_lattice(cells);
    u64::from(n[0]) * u64::from(pool.min(n[1])) * u64::from(n[2])
}

/// The sites of one fill box above the pool, clipped to the lattice: what the
/// kernel enumerates of it after the pool and the boxes before it.
pub(crate) fn box_count(cells: [u32; 3], pool: u32, sites: [[u32; 2]; 3]) -> u64 {
    let n = site_lattice(cells);
    let pool = pool.min(n[1]);
    let clip = |d: usize, lo: u32| {
        let a = lo.min(n[d]);
        let b = sites[d][1].min(n[d]);
        u64::from(b.saturating_sub(a))
    };
    clip(0, sites[0][0]) * clip(1, sites[1][0].max(pool)) * clip(2, sites[2][0])
}

/// A fill table holding one box, Water `water`'s (`fill_boxes`), for proofs
/// that run the fill without its domain.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) fn one_box_table(sites: [[u32; 2]; 3], water: u32) -> Vec<u32> {
    let mut table = vec![0; 8 * manifold_water_liquid::MAX_WATERS as usize];
    let [[x0, x1], [y0, y1], [z0, z1]] = sites;
    let row = 8 * water as usize;
    table[row..row + 8].copy_from_slice(&[x0, x1, y0, y1, z0, z1, water, 0]);
    table
}

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FillUniforms {
    fill_min_x: f32,
    fill_min_y: f32,
    fill_min_z: f32,
    cell_size: f32,
    fill_cells_x: i32,
    fill_cells_y: i32,
    fill_cells_z: i32,
    pool_sites: i32,
    jitter: f32,
    seed: i32,
    body_count: i32,
    epoch: i32,
    closed_faces: i32,
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
    bounds_hi_x: f32,
    bounds_hi_y: f32,
    bounds_hi_z: f32,
    dispatch_count: u32,
}

manifold_node_engine::primitive! {
    name: LiquidFill,
    type_id: "node.liquid_fill",
    purpose: "Place a liquid's starting particles at rest, one per half-cell site of the box node.gpu_flip_domain places on whole world cells (fill_min_x/y/z its first corner in metres, fill_cells_x/y/z its cells per axis; site j at (1/4 + j/2) cells along each axis from the box's first cell, 2 sites per cell, so a site sits at the same world position whatever the box): every site below pool_sites, then each row of fill_boxes in turn (the domain's table, one row per Water's Initial Fill: its sites [x0, x1) × [max(y0, pool_sites), y1) × [z0, z1), its Water and its shaped fill's region row plus one, 0 for a box; fill_box_sites the boxes' sites in all, fill_version moving on whenever the table would seed differently), in lattice order. Each particle moves up to jitter / 4 cells each way by a hash of seed; 0 keeps the exact lattice. The radius is the sphere of an eighth of a cell, carrying the row's Water (a pool's is Water 0); ids count from 1. A site the FLIP engine's solid holds when the epoch starts is left dead, radius 0: the solid is the Wall faces of the Bounds box (bounds_lo/hi in metres as placed, closed_faces the Wall bits −X +X −Y +Y −Z +Z; none by default) and the first body_count bodies at their poses, the smaller of the two at each node of a grid 1.5 cells outside the box, read trilinearly from that grid's nodes. A shaped fill's row names its region: a site of it outside that row of regions (its shape in shapes and atlas, its pose at the epoch's start) is left dead too, so the fill takes the shape. Only the live sites are kept, in site order, ids unchanged, in storage of exactly their count; count is the live sites, the slots a liquid starts with (node.liquid_state grows its own storage for sources). A fill past the 16,777,216 particles a count carries exactly is refused.",
    inputs: {
        fill_min_x: ScalarF32 optional, fill_min_y: ScalarF32 optional, fill_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        fill_cells_x: ScalarF32 optional, fill_cells_y: ScalarF32 optional, fill_cells_z: ScalarF32 optional,
        pool_sites: ScalarF32 optional,
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        collider_nodes: Array(manifold_node_engine::mesh::Vec4Vertex) optional,
        collider_triangles: Array(manifold_node_engine::mesh::Vec4Vertex) optional,
        regions: Array(LiquidBody) optional,
        fill_boxes: Array(u32) optional,
        fill_box_sites: ScalarF32 optional,
        fill_version: ScalarF32 optional,
        body_count: ScalarF32 optional,
        epoch: ScalarF32 optional,
        closed_faces: ScalarF32 optional,
        bounds_lo_x: ScalarF32 optional, bounds_lo_y: ScalarF32 optional, bounds_lo_z: ScalarF32 optional,
        bounds_hi_x: ScalarF32 optional, bounds_hi_y: ScalarF32 optional, bounds_hi_z: ScalarF32 optional,
    },
    outputs: {
        particles: Array(FluidParticle),
        count: ScalarF32,
    },
    params: [
        float_param!("fill_min_x", "Fill Min X", -2.0, -1.0e4, 1.0e4),
        float_param!("fill_min_y", "Fill Min Y", 0.0, -1.0e4, 1.0e4),
        float_param!("fill_min_z", "Fill Min Z", -2.0, -1.0e4, 1.0e4),
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        int_param!("fill_cells_x", "Fill Cells X", 64.0, 1.0, 16_777_216.0),
        int_param!("fill_cells_y", "Fill Cells Y", 64.0, 1.0, 16_777_216.0),
        int_param!("fill_cells_z", "Fill Cells Z", 64.0, 1.0, 16_777_216.0),
        int_param!("pool_sites", "Pool Height (half cells)", 5.0, 0.0, 2048.0),
        float_param!("jitter", "Jitter", 0.0, 0.0, 1.0),
        int_param!("seed", "Seed", 0.0, 0.0, 16_777_215.0),
        int_param!("body_count", "Bodies", 0.0, 0.0, MAX_LIQUID_BODIES as f32),
        int_param!("epoch", "Epoch", 0.0, 0.0, 16_777_215.0),
        int_param!("closed_faces", "Closed Faces (bits −X +X −Y +Y −Z +Z)", 0.0, 0.0, 63.0),
        float_param!("bounds_lo_x", "Bounds Min X", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_lo_y", "Bounds Min Y", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_lo_z", "Bounds Min Z", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_hi_x", "Bounds Max X", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_hi_y", "Bounds Max Y", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_hi_z", "Bounds Max Z", 0.0, -1.0e4, 1.0e4),
    ],
    depth_rule: Terminal,
    composition_notes: "Feeds node.liquid_state's seed; count is the filled sites that state starts its live slots from. The pool and box sites, the fill's box (fill_min, fill_cells, cell_size), the Bounds box and Wall faces, bodies, shapes, atlas, the collider trees, body_count and epoch come from the liquid's domain, so a Resolution change refills at the new size and a restart refills around the colliders' starting poses.",
    examples: [],
    picker: { label: "Liquid Fill", category: Atom },
    summary: "Places the liquid's starting particles: a pool on the floor plus one block of water.",
    category: Particles3D,
    role: Source,
    aliases: ["seed liquid", "initial water", "dam break fill", "pool"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/liquid_fill_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    wgsl_includes: [LIQUID_POSE, LIQUID_COLLIDER, COLLIDER_BVH, LIQUID_FILL_SHAPE, PARTICLE_WATER],
    extra_fields: {
        buffer: Option<GpuBuffer> = None,
        filled: Option<([u32; 21], usize)> = None,
        // The live sites the last fill kept.
        live: u32 = 0,
        compact: Option<(GpuComputePipeline, GpuComputePipeline)> = None,
    },
}

impl Primitive for LiquidFill {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "particles"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "particles")
            .then_some(self.buffer.as_ref())
            .flatten()
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, sized to the live sites at run
        // time.
        (port == "particles").then_some(1)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (min, cells, cell_size) =
            match fill_frame(|name, default| ctx.scalar_or_param(name, default)) {
                Ok(frame) => frame,
                Err(error) => {
                    ctx.error(format!("Liquid Fill: {error}"));
                    return;
                }
            };
        let int =
            |name: &str, default: f32| ctx.scalar_or_param(name, default).round().max(0.0) as u32;
        let jitter = ctx.scalar_or_param("jitter", 0.0);
        let closed_faces = ctx
            .scalar_or_param("closed_faces", 0.0)
            .round()
            .clamp(0.0, 63.0) as i32;
        let bounds = [
            "bounds_lo_x",
            "bounds_lo_y",
            "bounds_lo_z",
            "bounds_hi_x",
            "bounds_hi_y",
            "bounds_hi_z",
        ]
        .map(|name| ctx.scalar_or_param(name, 0.0));
        if !(0.0..=1.0).contains(&jitter) {
            ctx.error("Liquid Fill: jitter must be 0 to 1".to_string());
            return;
        }
        let pool = int("pool_sites", 5.0);
        let seed = int("seed", 0.0);
        let body_count = int("body_count", 0.0).min(MAX_LIQUID_BODIES as u32) as i32;
        // With colliders the sites depend on their pose at the epoch's start;
        // a shaped fill's own pose moves the table's version on.
        let epoch = if body_count > 0 { int("epoch", 0.0) } else { 0 };
        // The fill boxes after the pool, from the domain's table: none
        // without it.
        let boxes = ctx.inputs.array("fill_boxes");
        let box_sites = if boxes.is_some() {
            u64::from(int("fill_box_sites", 0.0))
        } else {
            0
        };
        let version = int("fill_version", 0.0);
        let filled = pool_count(cells, pool) + box_sites;
        if filled > u64::from(EXACT_F32_COUNT) {
            ctx.outputs.set_scalar("count", ParamValue::Float(0.0));
            ctx.error(format!(
                "Liquid Fill: the fill holds {filled} particles, more than the {EXACT_F32_COUNT} a particle count carries exactly"
            ));
            return;
        }
        let filled = filled as u32;
        let colliders = (
            ctx.inputs.array("bodies"),
            ctx.inputs.array("shapes"),
            ctx.inputs.array("atlas"),
            ctx.inputs.array("collider_nodes"),
            ctx.inputs.array("collider_triangles"),
        );
        let regions = ctx.inputs.array("regions");
        // Without all five collider arrays no body is read.
        let body_count = match colliders {
            (Some(bodies), Some(_), Some(_), Some(_), Some(_)) => body_count.min(
                (bodies.size / std::mem::size_of::<LiquidBody>() as u64).min(i32::MAX as u64)
                    as i32,
            ),
            _ => 0,
        };
        // Only a Wall face's plane decides a site: an open face's, which
        // follows the bodies when Bounds is off, never refills.
        let face = |i: usize| {
            let bit = if i < 3 { 2 * i } else { 2 * (i - 3) + 1 };
            if closed_faces & (1 << bit) != 0 {
                bounds[i].to_bits()
            } else {
                0
            }
        };
        let key = [
            min[0].to_bits(),
            min[1].to_bits(),
            min[2].to_bits(),
            cell_size.to_bits(),
            cells[0],
            cells[1],
            cells[2],
            pool,
            box_sites as u32,
            version,
            jitter.to_bits(),
            seed,
            body_count as u32,
            epoch,
            closed_faces as u32,
            face(0),
            face(1),
            face(2),
            face(3),
            face(4),
            face(5),
        ];
        if let Some(kept) = &self.buffer
            && self.filled == Some((key, kept.identity_key()))
        {
            ctx.outputs
                .set_scalar("count", ParamValue::Float(self.live as f32));
            return;
        }
        let gpu = ctx.gpu_encoder();
        // Every filled site, live or dead, before the live ones are kept.
        let bytes = u64::from(filled.max(1)) * std::mem::size_of::<FluidParticle>() as u64;
        let all_sites = match gpu.device.try_create_buffer(bytes, BufferUse::Gpu) {
            Ok(buffer) => buffer,
            Err(error) => {
                ctx.error(format!(
                    "Liquid Fill needs {bytes} bytes of GPU scratch: {error}"
                ));
                return;
            }
        };
        let buffer = &all_sites;
        // The particle buffer stands in for the arrays that are not read.
        let (bodies, shapes, atlas, nodes, triangles) = match colliders {
            (Some(bodies), Some(shapes), Some(atlas), Some(nodes), Some(triangles)) => {
                (bodies, shapes, atlas, nodes, triangles)
            }
            _ => (buffer, buffer, buffer, buffer, buffer),
        };
        // A shaped fill's region row is read beside the collider shapes.
        let regions = match (regions, colliders.1) {
            (Some(regions), Some(_)) => regions,
            _ => buffer,
        };
        let boxes = boxes.unwrap_or(buffer);
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let clamp = |v: u32| v.min(i32::MAX as u32) as i32;
        let uniforms = FillUniforms {
            fill_min_x: min[0],
            fill_min_y: min[1],
            fill_min_z: min[2],
            cell_size,
            fill_cells_x: clamp(cells[0]),
            fill_cells_y: clamp(cells[1]),
            fill_cells_z: clamp(cells[2]),
            pool_sites: clamp(pool),
            jitter,
            seed: clamp(seed),
            body_count,
            epoch: clamp(epoch),
            closed_faces,
            bounds_lo_x: bounds[0],
            bounds_lo_y: bounds[1],
            bounds_lo_z: bounds[2],
            bounds_hi_x: bounds[3],
            bounds_hi_y: bounds[4],
            bounds_hi_z: bounds[5],
            dispatch_count: filled,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: bodies,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: shapes,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: atlas,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: nodes,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: triangles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: regions,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 7,
                    buffer: boxes,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 8,
                    buffer,
                    offset: 0,
                },
            ],
            [filled.max(1).div_ceil(256), 1, 1],
            "node.liquid_fill",
        );
        match self.keep_live(gpu, &all_sites, filled) {
            Ok(kept) => {
                self.filled = Some((key, kept));
                ctx.outputs
                    .set_scalar("count", ParamValue::Float(self.live as f32));
            }
            Err(error) => {
                self.filled = None;
                ctx.outputs.set_scalar("count", ParamValue::Float(0.0));
                ctx.error(format!("Liquid Fill: {error}"));
            }
        }
    }
}

impl LiquidFill {
    /// Keep the live ones of the `filled` sites' particles in `all_sites`,
    /// in site order, in storage of exactly their count. The count is read
    /// back at once:
    /// the frame's work so far is committed and waited on, once per fill.
    /// Returns the storage's identity.
    fn keep_live(
        &mut self,
        gpu: &mut manifold_node_engine::gpu::gpu_encoder::GpuEncoder<'_>,
        all_sites: &GpuBuffer,
        filled: u32,
    ) -> Result<usize, String> {
        let device = gpu.device;
        let (flag, scatter) = self.compact.get_or_insert_with(|| {
            (
                device.create_compute_pipeline(COMPACT_SHADER, "flag", "node.liquid_fill.flag"),
                device.create_compute_pipeline(
                    COMPACT_SHADER,
                    "scatter",
                    "node.liquid_fill.scatter",
                ),
            )
        });
        let n = filled as usize;
        let mut scan = PrefixScan::default();
        scan.prepare(device);
        let params = [filled, 0, 0, 0];
        let groups = [filled.max(1).div_ceil(256), 1, 1];
        let live = if n == 0 {
            0
        } else {
            scan.buffer(device, n)?;
            let storage = scan.buffer_ref();
            let enc = &mut *gpu.native_enc;
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                flag,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::cast_slice(&params),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: all_sites,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: storage,
                        offset: 0,
                    },
                ],
                groups,
                "node.liquid_fill.flag",
            );
            enc.compute_memory_barrier_buffers();
            scan.encode_labelled(enc, n, COMPACT_LABELS);
            enc.commit_wait_and_continue(device);
            let source = storage
                .mapped_ptr()
                .ok_or("the live count is not CPU-visible")?
                .cast::<u32>()
                .cast_const();
            // SAFETY: shared storage of at least `n` words whose writers the
            // wait above has retired.
            unsafe { source.add(n - 1).read() }
        };
        self.live = live;
        let slots = live.max(1);
        let bytes = u64::from(slots) * std::mem::size_of::<FluidParticle>() as u64;
        if self.buffer.as_ref().is_none_or(|b| b.size != bytes) {
            manifold_node_engine::load::expand::admit_world_bytes(
                device.modifier_memory_snapshot(),
                bytes,
            )
            .map_err(|error| error.to_string())?;
            self.buffer = Some(
                device
                    .try_create_buffer(bytes, BufferUse::Gpu)
                    .map_err(|error| format!("{bytes} bytes of GPU storage: {error}"))?,
            );
        }
        let kept = self.buffer.as_ref().expect("fill storage prepared");
        let enc = &mut *gpu.native_enc;
        enc.clear_buffer(kept);
        if n > 0 {
            let storage = scan.buffer_ref();
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                scatter,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::cast_slice(&params),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: all_sites,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: storage,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: kept,
                        offset: 0,
                    },
                ],
                groups,
                "node.liquid_fill.scatter",
            );
            enc.compute_memory_barrier_buffers();
        }
        Ok(kept.identity_key())
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquid_fill_counts_pool_plus_clipped_box() {
        // Pool 5 site layers of a 64³ floor (128 × 128 sites), box x 5..43,
        // y 0..67 clipped to 5..67, z 8..120.
        assert_eq!(
            pool_count([64; 3], 5) + box_count([64; 3], 5, [[5, 43], [0, 67], [8, 120]]),
            128 * 5 * 128 + 38 * 62 * 112
        );
        // An empty box adds nothing; the pool never passes the lattice top.
        assert_eq!(
            pool_count([8; 3], 40) + box_count([8; 3], 40, [[0, 0], [0, 0], [0, 0]]),
            16 * 16 * 16
        );
        // A box past the lattice is clipped to it.
        assert_eq!(
            pool_count([8; 3], 0) + box_count([8; 3], 0, [[12, 40], [0, 16], [0, 16]]),
            4 * 16 * 16
        );
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
