//! `node.liquid_fill` — a liquid's starting particles: a pool on the floor
//! plus one box of cells, eight particles per cell, at rest
//! (docs/FFT_WATER_SOLVER_DESIGN.md P3). A pure function of its params, so it
//! owns its storage and fills it once per change. A source atom on the
//! codegen path.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer};

use super::collar_cells::cell_lattice;
use super::sort_particles_into_cells::{float_param, int_param};
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Particles per filled cell.
pub(super) const PER_CELL: u64 = 8;

/// The cells a pool plus a box clipped above it fill, in the kernel's
/// enumeration: `pool` rows of every column, then the box.
pub(super) fn filled_cells(nodes: [u32; 3], pool: u32, column: [[u32; 2]; 3]) -> u64 {
    let pool = pool.min(nodes[1]);
    let clip = |d: usize, lo: u32| {
        let a = lo.min(nodes[d]);
        let b = column[d][1].min(nodes[d]);
        u64::from(b.saturating_sub(a))
    };
    let pool_cells = u64::from(nodes[0]) * u64::from(pool) * u64::from(nodes[2]);
    pool_cells + clip(0, column[0][0]) * clip(1, column[1][0].max(pool)) * clip(2, column[2][0])
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
    pool_cells: i32,
    column_x0: i32,
    column_x1: i32,
    column_y0: i32,
    column_y1: i32,
    column_z0: i32,
    column_z1: i32,
    seed: i32,
    max_capacity: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: LiquidFill,
    type_id: "node.liquid_fill",
    purpose: "Place a liquid's starting particles at rest: every lattice cell below pool_cells, then the box of cells [column_x0, x1) × [max(column_y0, pool), y1) × [column_z0, z1), in lattice order, eight particles per cell, one per half-cell jittered by a hash of seed. The radius is the sphere of an eighth of a cell; ids count from 1. Holds max_capacity slots; slots past the fill are unused (radius 0), and a fill larger than the capacity is cut short. count is the number placed.",
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
        int_param!("pool_cells", "Pool Height (cells)", 3.0, 0.0, 1024.0),
        int_param!("column_x0", "Box Min X (cell)", 0.0, 0.0, 1024.0),
        int_param!("column_x1", "Box Max X (cell)", 0.0, 0.0, 1024.0),
        int_param!("column_y0", "Box Min Y (cell)", 0.0, 0.0, 1024.0),
        int_param!("column_y1", "Box Max Y (cell)", 0.0, 0.0, 1024.0),
        int_param!("column_z0", "Box Min Z (cell)", 0.0, 0.0, 1024.0),
        int_param!("column_z1", "Box Max Z (cell)", 0.0, 0.0, 1024.0),
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
        filled: Option<([u32; 16], usize)> = None,
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
        if !(cell_size.is_finite() && cell_size > 0.0) || min.iter().any(|v| !v.is_finite()) {
            ctx.error("Liquid Fill: the lattice box must be finite with a positive cell".to_string());
            return;
        }
        let pool = int("pool_cells", 3.0);
        let column = [
            [int("column_x0", 0.0), int("column_x1", 0.0)],
            [int("column_y0", 0.0), int("column_y1", 0.0)],
            [int("column_z0", 0.0), int("column_z1", 0.0)],
        ];
        let seed = int("seed", 0.0);
        let capacity = max_capacity(ctx.params);
        let placed = (filled_cells(nodes, pool, column) * PER_CELL).min(u64::from(capacity));
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
            column[0][0], column[0][1], column[1][0], column[1][1], column[2][0], column[2][1],
            seed, capacity,
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
            pool_cells: clamp(pool),
            column_x0: clamp(column[0][0]),
            column_x1: clamp(column[0][1]),
            column_y0: clamp(column[1][0]),
            column_y1: clamp(column[1][1]),
            column_z0: clamp(column[2][0]),
            column_z1: clamp(column[2][1]),
            seed: clamp(seed),
            max_capacity: clamp(capacity),
            dispatch_count: capacity,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
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
        // The P1 Dam Break scene: pool 3 cells, box x 10..29, y 0..33 (clipped
        // to 3..33), z 4..60.
        assert_eq!(filled_cells([64; 3], 3, [[10, 29], [0, 33], [4, 60]]), 64 * 3 * 64 + 19 * 30 * 56);
        // An empty box adds nothing; the pool never passes the lattice top.
        assert_eq!(filled_cells([8; 3], 20, [[0, 0], [0, 0], [0, 0]]), 8 * 8 * 8);
        // A box past the lattice is clipped to it.
        assert_eq!(filled_cells([8; 3], 0, [[6, 20], [0, 8], [0, 8]]), 2 * 8 * 8);
    }
}
