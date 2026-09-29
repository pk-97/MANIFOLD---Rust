//! `node.sort_particles_into_cells` — counting sort of liquid particles into
//! spatial bins (GPU_FLUID_SURFACE_DESIGN.md D17): count per bin, prefix-sum
//! the counts, scatter. One atom because none of its passes is barrier-free
//! and none has another consumer; the scan is shared with `node.running_total`
//! (`prefix_scan.rs`). Hand kernels under exclusion 1 of ADDING_PRIMITIVES.md,
//! as `node.spawn_from_mesh`.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use super::prefix_scan::PrefixScan;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::{CellRange, FluidParticle, bin_counts};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const SHADER: &str = include_str!("shaders/sort_particles_into_cells.wgsl");
const ENTRIES: [&str; 6] = ["clear_counts", "count_particles", "write_ranges", "clear_tail", "scatter", "stabilise"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SortParams {
    bin_min: [f32; 3],
    inv_cell: f32,
    bins: [u32; 3],
    count: u32,
    bin_total: u32,
    sorted_capacity: u32,
    write_order: u32,
    write_sorted: u32,
}

macro_rules! float_param {
    ($name:literal, $label:literal, $default:expr, $min:expr, $max:expr) => {
        ParamDef {
            name: Cow::Borrowed($name),
            label: $label,
            ty: ParamType::Float,
            default: ParamValue::Float($default),
            range: Some(($min, $max)),
            enum_values: &[],
        }
    };
}
pub(crate) use float_param;

crate::primitive! {
    name: SortParticlesIntoCells,
    type_id: "node.sort_particles_into_cells",
    purpose: "Sort liquid particles into a grid of spatial bins covering a box, so neighbour searches read only nearby bins. Outputs the particles in bin order (inactive records past the live total) and each bin's start and count. Within a bin, particles keep their input order, so every output is the same on every run. `order` gives each sorted slot's input index (0xffffffff past the live total), for consumers that keep their own per-particle arrays in input order. Either `sorted` or `order` may be left unwired. With `enabled` 0 it does nothing and every output keeps its contents. Bins are cell_size metres; bin (i, j, k) spans min + (i, j, k)·cell_size from the box's minimum corner, max(1, ceil(size / cell_size)) bins per axis.",
    inputs: {
        particles: Array(FluidParticle) required,
        count: ScalarF32 optional,
        enabled: ScalarF32 optional,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
    },
    outputs: {
        sorted: Array(FluidParticle),
        cell_ranges: Array(CellRange),
        order: Array(u32),
    },
    params: [
        float_param!("enabled", "Enabled", 1.0, 0.0, 1.0),
        float_param!("center_x", "Center X", 0.0, -1000.0, 1000.0),
        float_param!("center_y", "Center Y", 0.0, -1000.0, 1000.0),
        float_param!("center_z", "Center Z", 0.0, -1000.0, 1000.0),
        float_param!("size_x", "Size X", 4.0, 0.001, 1000.0),
        float_param!("size_y", "Size Y", 4.0, 0.001, 1000.0),
        float_param!("size_z", "Size Z", 4.0, 0.001, 1000.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.001, 100.0),
        ParamDef {
            name: Cow::Borrowed("max_cells"),
            label: "Max Cells",
            ty: ParamType::Int,
            default: ParamValue::Float(1_048_576.0),
            range: Some((1.0, 16_777_216.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire count from the producer's live count (a fluid frame's count_b) so stale records past it are never sorted; records with radius 0 are skipped. Wire the box from a lattice's bounds through node.transform_components (position = centre, scale = size). Every atom that searches these bins must use the same box and cell_size, so wire one value into all of them. A box needing more bins than Max Cells is a named error. Inside a substep region, gate it with the boundary's tick_end so it sorts once per tick.",
    examples: [],
    picker: { label: "Sort Particles Into Cells", category: Atom },
    summary: "Groups liquid particles by where they are, so later steps can find each particle's neighbours quickly.",
    category: Particles3D,
    role: Filter,
    aliases: ["bin particles", "spatial hash", "counting sort", "neighbour grid"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        pipelines: Vec<GpuComputePipeline> = Vec::new(),
        scan: PrefixScan = PrefixScan::default(),
        rank: Option<GpuBuffer> = None,
        slot_input: Option<GpuBuffer> = None,
    },
}

impl Primitive for SortParticlesIntoCells {
    fn array_output_capacity(
        &self,
        port: &str,
        params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        match port {
            "sorted" | "order" => inputs.iter().find(|(name, _)| *name == "particles").map(|&(_, n)| n),
            "cell_ranges" => match params.get("max_cells") {
                Some(ParamValue::Float(n)) => Some(n.clamp(1.0, 16_777_216.0) as u32),
                _ => Some(1_048_576),
            },
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Unwired, every particle slot: a numeric default would cap it at a
        // float's exact-integer range.
        let requested = match ctx.inputs.scalar("count") {
            Some(ParamValue::Float(count)) => Some(count),
            _ => None,
        };
        let center = ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let size = ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        {
            let gpu = ctx.gpu_encoder();
            if self.pipelines.is_empty() {
                for entry in ENTRIES {
                    self.pipelines.push(gpu.device.create_compute_pipeline(
                        SHADER,
                        entry,
                        "node.sort_particles_into_cells",
                    ));
                }
            }
            self.scan.prepare(gpu.device);
        }
        if ctx.scalar_or_param("enabled", 1.0) <= 0.5 {
            return;
        }
        let invalid_box = !(cell_size.is_finite() && cell_size > 0.0)
            || center.iter().chain(&size).any(|v| !v.is_finite())
            || size.iter().any(|v| *v <= 0.0);
        // A producer without a frame yet publishes no particles and no lattice
        // (its derived cell size is not positive); there is nothing to sort.
        if invalid_box && requested == Some(0.0) {
            return;
        }
        if invalid_box || requested.is_some_and(|count| !count.is_finite()) {
            ctx.error("Sort Particles Into Cells: box and cell size must be finite and positive");
            return;
        }
        let (Some(particles), Some(ranges)) = (ctx.inputs.array("particles"), ctx.outputs.array("cell_ranges")) else {
            return;
        };
        // Either per-slot output may be unwired; the slots are those every wired one holds.
        let sorted = ctx.outputs.array("sorted");
        let order = ctx.outputs.array("order");
        let particle_size = std::mem::size_of::<FluidParticle>() as u64;
        let capacity = (particles.size / particle_size) as u32;
        let sorted_capacity = [sorted.map(|b| b.size / particle_size), order.map(|b| b.size / 4)]
            .into_iter()
            .flatten()
            .fold(u64::from(capacity), u64::min) as u32;
        let count = requested.map_or(capacity, |count| (count.max(0.0) as u32).min(capacity)).min(sorted_capacity);
        let bins = bin_counts(size, cell_size);
        let bin_total = bins.iter().map(|&n| u64::from(n)).product::<u64>();
        let range_capacity = ranges.size / std::mem::size_of::<CellRange>() as u64;
        if bin_total > range_capacity {
            ctx.error(format!(
                "Sort Particles Into Cells needs {bin_total} cells for this box and cell size; Max Cells is {range_capacity}. Raise Max Cells or the cell size."
            ));
            return;
        }
        let bin_total = bin_total as u32;
        let gpu = ctx.gpu_encoder();
        let cell_counts = match self.scan.buffer(gpu.device, bin_total as usize) {
            Ok(buffer) => buffer.clone(),
            Err(error) => {
                ctx.error(format!("Sort Particles Into Cells: {error}"));
                return;
            }
        };
        let rank_bytes = u64::from(capacity.max(1)) * 4;
        if self.rank.as_ref().is_none_or(|rank| rank.size < rank_bytes) {
            self.rank = Some(gpu.device.create_buffer(rank_bytes));
        }
        if self.slot_input.as_ref().is_none_or(|slots| slots.size < rank_bytes) {
            self.slot_input = Some(gpu.device.create_buffer(rank_bytes));
        }
        let rank = self.rank.as_ref().expect("rank scratch allocated");
        let slot_input = self.slot_input.as_ref().expect("slot scratch allocated");
        let uniforms = SortParams {
            bin_min: std::array::from_fn(|axis| center[axis] - 0.5 * size[axis]),
            inv_cell: 1.0 / cell_size,
            bins,
            count,
            bin_total,
            sorted_capacity,
            write_order: u32::from(order.is_some()),
            write_sorted: u32::from(sorted.is_some()),
        };
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
            // Unwired outputs are never written (their write flag is 0); rank keeps
            // the layout bound.
            GpuBinding::Buffer { binding: 2, buffer: sorted.unwrap_or(rank), offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: ranges, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: &cell_counts, offset: 0 },
            GpuBinding::Buffer { binding: 5, buffer: rank, offset: 0 },
            GpuBinding::Buffer { binding: 6, buffer: order.unwrap_or(rank), offset: 0 },
            GpuBinding::Buffer { binding: 7, buffer: slot_input, offset: 0 },
        ];
        let groups = |n: u32| [n.div_ceil(256).max(1), 1, 1];
        let encoder = &mut *gpu.native_enc;
        let [clear, count_pass, write_ranges, clear_tail, scatter, stabilise] = &self.pipelines[..] else {
            unreachable!("six sort pipelines");
        };
        encoder.dispatch_compute(clear, &bindings, groups(bin_total), "node.sort_particles_into_cells.clear");
        encoder.compute_memory_barrier_buffers();
        encoder.dispatch_compute(count_pass, &bindings, groups(count), "node.sort_particles_into_cells.count");
        encoder.compute_memory_barrier_buffers();
        self.scan.encode(encoder, bin_total as usize);
        encoder.dispatch_compute(write_ranges, &bindings, groups(bin_total), "node.sort_particles_into_cells.ranges");
        encoder.dispatch_compute(clear_tail, &bindings, groups(sorted_capacity), "node.sort_particles_into_cells.tail");
        encoder.compute_memory_barrier_buffers();
        encoder.dispatch_compute(scatter, &bindings, groups(count), "node.sort_particles_into_cells.scatter");
        encoder.compute_memory_barrier_buffers();
        encoder.dispatch_compute(stabilise, &bindings, groups(bin_total), "node.sort_particles_into_cells.stabilise");
    }
}
