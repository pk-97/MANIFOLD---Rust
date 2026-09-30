//! `node.liquid_fill` — a liquid's starting particles: a pool on the floor
//! plus one box, one particle per half-cell site, at rest
//! (docs/FFT_WATER_SOLVER_DESIGN.md P3). The sites are the FLIP Fluids
//! engine's seeding lattice, so both solvers start from the same water. A
//! pure function of its params, so it owns its storage and fills it once
//! per change. A source atom on the codegen path.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer};

use super::collar_cells::cell_lattice;
use super::sort_particles_into_cells::{float_param, int_param};
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Sites per axis of a lattice with `nodes` cells: two per cell.
fn site_lattice(nodes: [u32; 3]) -> [u32; 3] {
    nodes.map(|n| 2 * n)
}

/// The half-open site range [j0, j1) along one axis whose points
/// `min + (1/4 + j/2)·cell_size` lie in [lo, hi): the engine's rule for a box
/// of liquid (`_addNewFluidCellsAABB`, `AABB::isPointInside`). The preset
/// builder, which runs in tests, turns the engine's metres into sites.
#[cfg(test)]
pub(super) fn site_range(lo: f64, hi: f64, min: f64, cell_size: f64, cells: u32) -> [u32; 2] {
    let first = |x: f64| (2.0 * (x - min) / cell_size - 0.5).ceil().clamp(0.0, f64::from(2 * cells)) as u32;
    [first(lo), first(hi).max(first(lo))]
}

/// The sites a pool plus a box clipped above it fill, in the kernel's
/// enumeration: `pool` site layers of the whole floor, then the box.
pub(super) fn filled_sites(nodes: [u32; 3], pool: u32, sites: [[u32; 2]; 3]) -> u64 {
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

fn max_capacity(params: &ParamValues) -> u32 {
    match params.get("max_capacity") {
        Some(ParamValue::Float(n)) => n.clamp(8.0, 67_108_864.0) as u32,
        _ => 400_000,
    }
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
    max_capacity: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: LiquidFill,
    type_id: "node.liquid_fill",
    purpose: "Place a liquid's starting particles at rest, one per half-cell site (site j at (1/4 + j/2) cells along each axis, 2·nodes sites per axis): every site below pool_sites, then the box of sites [box_x0, x1) × [max(box_y0, pool_sites), y1) × [box_z0, z1), in lattice order. Each particle moves up to jitter / 4 cells each way by a hash of seed; 0 keeps the exact lattice. The radius is the sphere of an eighth of a cell; ids count from 1. Holds max_capacity slots; slots past the fill are unused (radius 0), and a fill larger than the capacity is cut short. count is the number placed.",
    inputs: {},
    outputs: {
        particles: Array(FluidParticle),
        count: ScalarF32,
    },
    params: [
        float_param!("lattice_min_x", "Lattice Min X", -2.0, -1.0e4, 1.0e4),
        float_param!("lattice_min_y", "Lattice Min Y", 0.0, -1.0e4, 1.0e4),
        float_param!("lattice_min_z", "Lattice Min Z", -2.0, -1.0e4, 1.0e4),
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
        int_param!("pool_sites", "Pool Height (half cells)", 5.0, 0.0, 2048.0),
        int_param!("box_x0", "Box Min X (half cell)", 0.0, 0.0, 2048.0),
        int_param!("box_x1", "Box Max X (half cell)", 0.0, 0.0, 2048.0),
        int_param!("box_y0", "Box Min Y (half cell)", 0.0, 0.0, 2048.0),
        int_param!("box_y1", "Box Max Y (half cell)", 0.0, 0.0, 2048.0),
        int_param!("box_z0", "Box Min Z (half cell)", 0.0, 0.0, 2048.0),
        int_param!("box_z1", "Box Max Z (half cell)", 0.0, 0.0, 2048.0),
        float_param!("jitter", "Jitter", 0.0, 0.0, 1.0),
        int_param!("seed", "Seed", 0.0, 0.0, 16_777_215.0),
        int_param!("max_capacity", "Max Particles", 400_000.0, 8.0, 67_108_864.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Feeds node.liquid_feedback's seed and, through count, every atom that takes a live particle count. The lattice must be the one the FFT water step uses.",
    examples: [],
    picker: { label: "Liquid Fill", category: Atom },
    summary: "Places the liquid's starting particles: a pool on the floor plus one block of water.",
    category: Particles3D,
    role: Source,
    aliases: ["seed liquid", "initial water", "dam break fill", "pool"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/liquid_fill_body.wgsl"),
    input_access: [],
    extra_fields: {
        buffer: Option<GpuBuffer> = None,
        filled: Option<([u32; 17], usize)> = None,
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
        (port == "particles").then(|| max_capacity(params))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Liquid Fill: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let float = |name: &str, default: f32| ctx.scalar_or_param(name, default);
        let int = |name: &str, default: f32| ctx.scalar_or_param(name, default).round().max(0.0) as u32;
        let min = [float("lattice_min_x", -2.0), float("lattice_min_y", 0.0), float("lattice_min_z", -2.0)];
        let cell_size = float("cell_size", 0.0625);
        let jitter = float("jitter", 0.0);
        if !(cell_size.is_finite() && cell_size > 0.0) || min.iter().any(|v| !v.is_finite()) {
            ctx.error("Liquid Fill: the lattice box must be finite with a positive cell".to_string());
            return;
        }
        if !(0.0..=1.0).contains(&jitter) {
            ctx.error("Liquid Fill: jitter must be 0 to 1".to_string());
            return;
        }
        let pool = int("pool_sites", 5.0);
        let sites = [
            [int("box_x0", 0.0), int("box_x1", 0.0)],
            [int("box_y0", 0.0), int("box_y1", 0.0)],
            [int("box_z0", 0.0), int("box_z1", 0.0)],
        ];
        let seed = int("seed", 0.0);
        let capacity = max_capacity(ctx.params);
        let placed = filled_sites(nodes, pool, sites).min(u64::from(capacity));
        ctx.outputs.set_scalar("count", ParamValue::Float(placed as f32));
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
        let key = [
            min[0].to_bits(), min[1].to_bits(), min[2].to_bits(), cell_size.to_bits(),
            nodes[0], nodes[1], nodes[2], pool,
            sites[0][0], sites[0][1], sites[1][0], sites[1][1], sites[2][0], sites[2][1],
            jitter.to_bits(), seed, capacity,
        ];
        let gpu = ctx.gpu_encoder();
        let buffer = self.buffer.as_ref().expect("fill storage prepared");
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
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            pool_sites: clamp(pool),
            box_x0: clamp(sites[0][0]),
            box_x1: clamp(sites[0][1]),
            box_y0: clamp(sites[1][0]),
            box_y1: clamp(sites[1][1]),
            box_z0: clamp(sites[2][0]),
            box_z1: clamp(sites[2][1]),
            jitter,
            seed: clamp(seed),
            max_capacity: clamp(capacity),
            dispatch_count: capacity,
            _pad0: 0,
            _pad1: 0,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer, offset: 0 },
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
