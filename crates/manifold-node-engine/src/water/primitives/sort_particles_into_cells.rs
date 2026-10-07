//! `node.sort_particles_into_cells` — counting sort of liquid particles into
//! spatial bins (GPU_FLUID_SURFACE_DESIGN.md D17): count per bin, prefix-sum
//! the counts, scatter. One atom because none of its passes is barrier-free
//! and none has another consumer; the scan is shared with `node.running_total`
//! (`prefix_scan.rs`). Hand kernels under exclusion 1 of ADDING_PRIMITIVES.md,
//! as `node.spawn_from_mesh`.
//!
//! The particles port accepts any record that names where a point is and
//! whether it is live (`Channels[permissive]`, CHANNEL_TYPE_SYSTEM.md section
//! 11.4 (Per-port match-mode discipline)): a liquid particle's
//! `position_radius`, live when the radius is positive, or a `position` and an
//! `id`, live when the id is non-zero and the position finite. So the matter
//! solver sorts its own points in place of a converted copy each tick.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use super::prefix_scan::{PrefixScan, ScanLabels};
use crate::channel_names::well_known;
use crate::exec::effect_node::{EffectNodeContext, ParamValues};
use crate::water::fluid_particles::{CellRange, FLUID_PARTICLE_SPECS, FluidParticle, MAX_BINS, bin_counts, bin_total, searched_bins};
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::ports::{ArrayType, ChannelElementType, std430_channel};
use crate::primitive::Primitive;
use crate::water::whitewater::WHITEWATER_EMPTY;

const SHADER: &str = include_str!("shaders/sort_particles_into_cells.wgsl");
const ENTRIES: [&str; 6] = ["clear_counts", "count_particles", "write_ranges", "clear_tail", "scatter", "stabilise"];

/// Live when the radius word is positive.
const LIVE_BY_RADIUS: u32 = 0;
/// Live when the id word is non-zero and the position finite.
const LIVE_BY_ID: u32 = 1;
/// Live when the kind word holds foam, bubble, spray or dust (4), excluding
/// [`WHITEWATER_EMPTY`], and the position is finite: dead particles count until
/// the tick's removal, as in FLIP.
const LIVE_BY_KIND: u32 = 2;
// The shader writes the empty kind as 3u.
const _: () = assert!(WHITEWATER_EMPTY == 3);

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
    stride_words: u32,
    position_word: u32,
    live_word: u32,
    live_rule: u32,
}

/// Where the sort reads a record's position and liveness, in 4-byte words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecordRead {
    stride_words: u32,
    position_word: u32,
    live_word: u32,
    live_rule: u32,
}

/// A liquid particle record: `position_radius` first, live when the radius is positive.
pub(crate) const LIQUID_PARTICLE_READ: RecordRead = RecordRead {
    stride_words: (std::mem::size_of::<FluidParticle>() / 4) as u32,
    position_word: 0,
    live_word: 3,
    live_rule: LIVE_BY_RADIUS,
};

/// Dispatch labels for a sort's six passes, so the profiler names its caller.
pub(crate) struct SortLabels {
    pub clear: &'static str,
    pub count: &'static str,
    pub scan: ScanLabels,
    pub ranges: &'static str,
    pub tail: &'static str,
    pub scatter: &'static str,
    pub stabilise: &'static str,
}

const NODE_LABELS: SortLabels = SortLabels {
    clear: "node.sort_particles_into_cells.clear",
    count: "node.sort_particles_into_cells.count",
    scan: ScanLabels { blocks: "node.sort_particles_into_cells.scan.blocks", add: "node.sort_particles_into_cells.scan.add" },
    ranges: "node.sort_particles_into_cells.ranges",
    tail: "node.sort_particles_into_cells.tail",
    scatter: "node.sort_particles_into_cells.scatter",
    stabilise: "node.sort_particles_into_cells.stabilise",
};

/// One sort: the records, the bin grid, and the per-slot outputs to write.
pub(crate) struct SortJob<'a> {
    pub particles: &'a GpuBuffer,
    pub read: RecordRead,
    /// Records the particles buffer holds.
    pub capacity: u32,
    /// Live records to sort, at most every slot of every output.
    pub count: u32,
    pub bin_min: [f32; 3],
    pub inv_cell: f32,
    pub bins: [u32; 3],
    /// Liquid particle records only; `None` leaves it unwritten.
    pub sorted: Option<&'a GpuBuffer>,
    pub order: Option<&'a GpuBuffer>,
    /// A GPU FLIP clock plan: an inactive slot's (live, no time to step)
    /// sort runs no pass and leaves every output as it was. `None` always sorts.
    pub gate: Option<&'a GpuBuffer>,
}

/// The counting sort itself, shared by `node.sort_particles_into_cells` and
/// the stage nodes that sort inside one dispatch chain (`node.gpu_flip_step`).
/// Owns the cell ranges and its scratch.
#[derive(Default)]
pub(crate) struct ParticleSorter {
    pipelines: Vec<GpuComputePipeline>,
    scan: PrefixScan,
    rank: Option<GpuBuffer>,
    slot_input: Option<GpuBuffer>,
    ranges: Option<GpuBuffer>,
    /// Zeros, the gate an ungated sort binds.
    open: Option<GpuBuffer>,
}

impl ParticleSorter {
    /// Create the pipelines. Call before any early return (compile contract).
    pub(crate) fn prepare(&mut self, device: &GpuDevice) {
        if self.pipelines.is_empty() {
            for entry in ENTRIES {
                self.pipelines.push(device.create_compute_pipeline(SHADER, entry, "node.sort_particles_into_cells"));
            }
        }
        self.scan.prepare(device);
        if self.open.is_none() {
            self.open = Some(super::prefix_scan::open_gate(device));
        }
    }

    /// One range per bin of `bins`, at `range_storage_bytes(bins)` or more.
    pub(crate) fn ranges(&self) -> Option<&GpuBuffer> {
        self.ranges.as_ref()
    }

    /// Size the cell ranges for `bins` before anything reads them.
    pub(crate) fn reserve_ranges(&mut self, device: &GpuDevice, bins: [u32; 3]) -> Result<(), String> {
        let range_bytes = range_storage_bytes(bins);
        if self.ranges.as_ref().is_some_and(|ranges| ranges.size >= range_bytes) {
            return Ok(());
        }
        let buffer = crate::load::expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            range_bytes,
        )
        .map_err(|error| error.to_string())
        .and_then(|()| device.try_create_buffer_shared(range_bytes))?;
        // Nothing reads a range the passes did not write as anything but empty.
        buffer.zero_fill();
        self.ranges = Some(buffer);
        Ok(())
    }

    /// Encode the six passes. `prepare` and `reserve_ranges(job.bins)` first.
    /// Fails before any dispatch when the scan storage cannot be had.
    pub(crate) fn encode(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        job: &SortJob<'_>,
        labels: &SortLabels,
    ) -> Result<(), String> {
        let bin_total = bin_total(job.bins) as u32;
        let cell_counts = self.scan.buffer(device, bin_total as usize)?.clone();
        let particle_size = std::mem::size_of::<FluidParticle>() as u64;
        let sorted_capacity = [job.sorted.map(|b| b.size / particle_size), job.order.map(|b| b.size / 4)]
            .into_iter()
            .flatten()
            .fold(u64::from(job.capacity), u64::min) as u32;
        let count = job.count.min(sorted_capacity);
        let rank_bytes = u64::from(job.capacity.max(1)) * 4;
        if self.rank.as_ref().is_none_or(|rank| rank.size < rank_bytes) {
            self.rank = Some(device.create_buffer(rank_bytes));
        }
        if self.slot_input.as_ref().is_none_or(|slots| slots.size < rank_bytes) {
            self.slot_input = Some(device.create_buffer(rank_bytes));
        }
        let ranges = self.ranges.as_ref().ok_or("the cell ranges were not reserved")?;
        let gate = job.gate.or(self.open.as_ref()).expect("sort pipelines prepared");
        let rank = self.rank.as_ref().expect("rank scratch allocated");
        let slot_input = self.slot_input.as_ref().expect("slot scratch allocated");
        let read = job.read;
        let uniforms = SortParams {
            bin_min: job.bin_min,
            inv_cell: job.inv_cell,
            bins: job.bins,
            count,
            bin_total,
            sorted_capacity,
            write_order: u32::from(job.order.is_some()),
            write_sorted: u32::from(job.sorted.is_some()),
            stride_words: read.stride_words,
            position_word: read.position_word,
            live_word: read.live_word,
            live_rule: read.live_rule,
        };
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: job.particles, offset: 0 },
            // Unwritten outputs (write flag 0) bind rank to keep the layout.
            GpuBinding::Buffer { binding: 2, buffer: job.sorted.unwrap_or(rank), offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: ranges, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: &cell_counts, offset: 0 },
            GpuBinding::Buffer { binding: 5, buffer: rank, offset: 0 },
            GpuBinding::Buffer { binding: 6, buffer: job.order.unwrap_or(rank), offset: 0 },
            GpuBinding::Buffer { binding: 7, buffer: slot_input, offset: 0 },
            GpuBinding::Buffer { binding: 8, buffer: gate, offset: 0 },
        ];
        let groups = |n: u32| [n.div_ceil(256).max(1), 1, 1];
        let [clear, count_pass, write_ranges, clear_tail, scatter, stabilise] = &self.pipelines[..] else {
            unreachable!("six sort pipelines");
        };
        encoder.dispatch_compute(clear, &bindings, groups(bin_total), labels.clear);
        encoder.compute_memory_barrier_buffers();
        encoder.dispatch_compute(count_pass, &bindings, groups(count), labels.count);
        encoder.compute_memory_barrier_buffers();
        match job.gate {
            Some(gate) => self.scan.encode_labelled_gated(encoder, bin_total as usize, labels.scan, gate),
            None => self.scan.encode_labelled(encoder, bin_total as usize, labels.scan),
        }
        encoder.dispatch_compute(write_ranges, &bindings, groups(bin_total), labels.ranges);
        encoder.dispatch_compute(clear_tail, &bindings, groups(sorted_capacity), labels.tail);
        encoder.compute_memory_barrier_buffers();
        encoder.dispatch_compute(scatter, &bindings, groups(count), labels.scatter);
        encoder.compute_memory_barrier_buffers();
        // One thread per scattered slot; there are at most `count`.
        encoder.dispatch_compute(stabilise, &bindings, groups(count), labels.stabilise);
        Ok(())
    }
}

/// How the sort reads a whitewater pool slot.
pub(crate) fn whitewater_record_read() -> RecordRead {
    record_read(&ArrayType::of_known::<crate::water::whitewater::WhitewaterParticle>())
        .expect("a whitewater slot carries a position and a kind")
}

fn record_read(layout: &ArrayType) -> Option<RecordRead> {
    let stride_words = layout.item_size / 4;
    if let Some((offset, ChannelElementType::Vec4F)) = std430_channel(layout.specs, well_known::POSITION_RADIUS) {
        return Some(RecordRead {
            stride_words,
            position_word: offset / 4,
            live_word: offset / 4 + 3,
            live_rule: LIVE_BY_RADIUS,
        });
    }
    if let (Some((position, ChannelElementType::Vec4F)), Some((kind, ChannelElementType::U32))) =
        (std430_channel(layout.specs, well_known::POSITION_LIFETIME), std430_channel(layout.specs, well_known::KIND))
    {
        return Some(RecordRead { stride_words, position_word: position / 4, live_word: kind / 4, live_rule: LIVE_BY_KIND });
    }
    match (std430_channel(layout.specs, well_known::POSITION), std430_channel(layout.specs, well_known::ID)) {
        (Some((position, ChannelElementType::Vec3F)), Some((id, ChannelElementType::U32))) => Some(RecordRead {
            stride_words,
            position_word: position / 4,
            live_word: id / 4,
            live_rule: LIVE_BY_ID,
        }),
        _ => None,
    }
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

/// An Int param: whole numbers in `[min, max]`, stored as a float.
macro_rules! int_param {
    ($name:literal, $label:literal, $default:expr, $min:expr, $max:expr) => {
        ParamDef {
            name: Cow::Borrowed($name),
            label: $label,
            ty: ParamType::Int,
            default: ParamValue::Float($default),
            range: Some(($min, $max)),
            enum_values: &[],
        }
    };
}
pub(crate) use int_param;

/// A searching atom's `bins_x/y/z` param, shadowed by the sort's output of the
/// same name. 0 until wired.
macro_rules! bin_param {
    ($name:literal, $label:literal) => {
        ParamDef {
            name: Cow::Borrowed($name),
            label: $label,
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 16_777_216.0)),
            enum_values: &[],
        }
    };
}
pub(crate) use bin_param;

/// The bin grid a searching atom indexes `cell_ranges` with, from its
/// `bins_x/y/z` inputs, checked against the ranges' storage before any
/// dispatch. `None` while the sort has no lattice yet (wired zeros). A graph
/// saved before the bins wires leaves all three unset: the sort's own CPU rule
/// on the box this atom shares with it gives the same grid, still checked.
pub(crate) fn read_searched_bins(
    ctx: &EffectNodeContext<'_, '_>,
    range_bytes: u64,
    atom: &str,
) -> Result<Option<[u32; 3]>, String> {
    let ports = ["bins_x", "bins_y", "bins_z"];
    let bins = ports.map(|port| ctx.scalar_or_param(port, 0.0));
    if bins == [0.0; 3] {
        if ports.iter().all(|port| ctx.inputs.scalar(port).is_some()) {
            return Ok(None);
        }
        if ports.iter().all(|port| ctx.inputs.scalar(port).is_none()) {
            let size = ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
            let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
            if !(cell_size.is_finite() && cell_size > 0.0) || size.iter().any(|v| !(v.is_finite() && *v > 0.0)) {
                return Ok(None);
            }
            return searched_bins(bin_counts(size, cell_size).map(|n| n as f32), range_bytes, atom).map(Some);
        }
    }
    searched_bins(bins, range_bytes, atom).map(Some)
}

crate::primitive! {
    name: SortParticlesIntoCells,
    type_id: "node.sort_particles_into_cells",
    purpose: "Sort liquid particles into a grid of spatial bins covering a box, so neighbour searches read only nearby bins. Outputs the particles in bin order (inactive records past the live total), each bin's start and count, and the bin grid's size per axis. Within a bin, particles keep their input order, so every output is the same on every run. `order` gives each sorted slot's input index (0xffffffff past the live total), for consumers that keep their own per-particle arrays in input order. Either `sorted` or `order` may be left unwired. With `enabled` 0 it does nothing and every output keeps its contents. Bins are cell_size metres; bin (i, j, k) spans min + (i, j, k)·cell_size from the box's minimum corner, max(1, ceil(size / cell_size)) bins per axis. cell_ranges holds exactly one range per bin, sized every frame from the same bin count. Particles may be liquid particle records (live when the radius is positive) or any record with a position and an id (live when the id is non-zero and the position finite), such as matter points.",
    inputs: {
        particles: Channels[permissive] required,
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
        bins_x: ScalarF32, bins_y: ScalarF32, bins_z: ScalarF32,
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
    ],
    depth_rule: Terminal,
    composition_notes: "Wire count from the producer's live count (a fluid frame's count_b) so stale records past it are never sorted; records with radius 0 are skipped. Matter points wire straight in from node.matter_state's out; `sorted` holds liquid particle records, so leave it unwired and use `order` when sorting anything else. Wire the box from a lattice's bounds through node.transform_components (position = centre, scale = size). Every atom that searches these bins must use the same box and cell_size, and must take bins_x/y/z from here rather than work the grid out itself: GPU division of the same floats can land one bin higher. bins_x/y/z are 0 while the producer has no lattice yet. A grid the device cannot hold is a named error. Inside a substep region, gate it with the boundary's tick_end so it sorts once per tick.",
    examples: [],
    picker: { label: "Sort Particles Into Cells", category: Atom },
    summary: "Groups liquid particles by where they are, so later steps can find each particle's neighbours quickly.",
    category: Particles3D,
    role: Filter,
    aliases: ["bin particles", "spatial hash", "counting sort", "neighbour grid"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        sorter: ParticleSorter = ParticleSorter::default(),
    },
}

/// Bytes of `cell_ranges` storage for a grid of `bins`: one range per bin,
/// and at least one so an empty grid still binds.
pub(crate) fn range_storage_bytes(bins: [u32; 3]) -> u64 {
    bin_total(bins).max(1) * std::mem::size_of::<CellRange>() as u64
}

impl Primitive for SortParticlesIntoCells {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "cell_ranges"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "cell_ranges").then_some(self.sorter.ranges()).flatten()
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        match port {
            "sorted" | "order" => inputs.iter().find(|(name, _)| *name == "particles").map(|&(_, n)| n),
            // Provided storage: a one-record hint, sized to the bin grid at run time.
            "cell_ranges" => Some(1),
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
        self.sorter.prepare(ctx.gpu_encoder().device);
        if ctx.scalar_or_param("enabled", 1.0) <= 0.5 {
            return;
        }
        let publish = |ctx: &mut EffectNodeContext<'_, '_>, bins: [u32; 3]| {
            for (port, n) in ["bins_x", "bins_y", "bins_z"].into_iter().zip(bins) {
                ctx.outputs.set_scalar(port, ParamValue::Float(n as f32));
            }
        };
        let invalid_box = !(cell_size.is_finite() && cell_size > 0.0)
            || center.iter().chain(&size).any(|v| !v.is_finite())
            || size.iter().any(|v| *v <= 0.0);
        if invalid_box || requested.is_some_and(|count| !count.is_finite()) {
            publish(ctx, [0; 3]);
            ctx.error("Sort Particles Into Cells: box and cell size must be finite and positive");
            return;
        }
        let bins = bin_counts(size, cell_size);
        let bin_total = bin_total(bins);
        if bin_total > MAX_BINS {
            publish(ctx, [0; 3]);
            ctx.error(format!(
                "Sort Particles Into Cells: a {}×{}×{} bin grid is more than the {MAX_BINS} bins a search can index. Raise the cell size.",
                bins[0], bins[1], bins[2]
            ));
            return;
        }
        // The ranges are sized from the same bin count every pass dispatches
        // over, before any of them is encoded.
        if let Err(error) = self.sorter.reserve_ranges(ctx.gpu_encoder().device, bins) {
            publish(ctx, [0; 3]);
            ctx.error(format!(
                "Sort Particles Into Cells: a {}×{}×{} bin grid needs {} bytes of cell ranges the device cannot give: {error}. Raise the cell size.",
                bins[0],
                bins[1],
                bins[2],
                range_storage_bytes(bins)
            ));
            return;
        }
        // The bins go out only once every pass below will run, so a searcher
        // never reads ranges this frame left unwritten.
        let Some(particles) = ctx.inputs.array("particles") else {
            publish(ctx, [0; 3]);
            return;
        };
        let refuse = |ctx: &mut EffectNodeContext<'_, '_>, error: String| {
            publish(ctx, [0; 3]);
            ctx.error(error);
        };
        let Some(layout) = ctx.inputs.array_layout("particles") else {
            refuse(ctx, "Sort Particles Into Cells: the particles wire carries no record layout".into());
            return;
        };
        let Some(read) = record_read(&layout) else {
            refuse(ctx, "Sort Particles Into Cells: particles need a position_radius channel, or position and id channels".into());
            return;
        };
        if ctx.outputs.array("sorted").is_some() && layout.specs != FLUID_PARTICLE_SPECS {
            refuse(ctx, "Sort Particles Into Cells: sorted holds liquid particle records; leave it unwired and use order for these points".into());
            return;
        }
        // Either per-slot output may be unwired; the sorter writes the slots every wired one holds.
        let capacity = (particles.size / u64::from(layout.item_size)) as u32;
        let count = requested.map_or(capacity, |count| (count.max(0.0) as u32).min(capacity));
        let sorted = ctx.outputs.array("sorted");
        let order = ctx.outputs.array("order");
        let job = SortJob {
            particles,
            read,
            capacity,
            count,
            bin_min: std::array::from_fn(|axis| center[axis] - 0.5 * size[axis]),
            inv_cell: 1.0 / cell_size,
            bins,
            sorted,
            order,
            gate: None,
        };
        let gpu = ctx.gpu_encoder();
        let encoded = self.sorter.encode(gpu.device, gpu.native_enc, &job, &NODE_LABELS);
        match encoded {
            Ok(()) => publish(ctx, bins),
            Err(error) => refuse(ctx, format!("Sort Particles Into Cells: {error}")),
        }
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
