//! `node.sort_particles_into_cells` — counting sort of particles into the
//! cells of a world-tile pool (GPU_FLUID_SURFACE_DESIGN.md D17): count per
//! cell, prefix-sum the counts, scatter. One atom because none of its passes
//! is barrier-free and none has another consumer; the scan is shared with
//! `node.running_total` (`prefix_scan.rs`). Hand kernels under exclusion 1 of
//! ADDING_PRIMITIVES.md, as `node.spawn_from_mesh`.
//!
//! The particles port accepts any record that names where a point is and
//! whether it is live: a liquid particle's
//! `position_radius`, live when the radius is positive, or a `position` and an
//! `id`, live when the id is non-zero and the position finite. So the matter
//! solver sorts its own points in place of a converted copy each tick.

use manifold_gpu::BufferUse;
use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use super::prefix_scan::{PrefixScan, ScanLabels};
use crate::fluid_particles::{CellRange, bin_total};
use crate::tile_pool::tile_pool;
use crate::whitewater::WHITEWATER_EMPTY;
use manifold_node_engine::channel_names::well_known;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::{FLUID_PARTICLE_SPECS, FluidParticle};
use manifold_node_engine::ports::{ArrayType, ChannelElementType, std430_channel};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, lattice_origin, pool_len, table_words};

/// The sort's passes. Composed after a key source: [`DENSE_KEYS`],
/// [`POOLED_KEYS`] or [`BLOCK_KEYS`] (the pooled ones with `READ_SIDE` before
/// them), which define `sort_key_of`.
pub const SHADER: &str = include_str!("shaders/sort_particles_into_cells.wgsl");

/// A point's key is the index of its bin in the grid, the point clamped into
/// the grid: one range per bin.
const DENSE_KEYS: &str = "fn sort_key_of(p: vec3<f32>) -> u32 {\n\
    let last = vec3<i32>(params.bins) - vec3<i32>(1);\n\
    let b = clamp(vec3<i32>(floor((p - params.bin_min) * params.inv_cell)), vec3<i32>(0), last);\n\
    return u32(b.x) + params.bins.x * (u32(b.y) + params.bins.y * u32(b.z));\n\
}\n";

/// A point's key is the pool index (`ts_cell_at`) of its world cell, counted
/// from the lattice origin with no clamp: one range per element of a pooled
/// field, the canonical element's included. The table is the world-tile
/// table; a point in a tile it does not hold keys to the canonical element.
const POOLED_KEYS: &str = "@group(0) @binding(9) var<storage, read> ts_table: array<u32>;\n\
fn ts_pool_tiles() -> u32 { return params.pool_tiles; }\n\
fn ts_table_word(i: u32) -> u32 { return ts_table[i]; }\n\
fn sort_key_of(p: vec3<f32>) -> u32 { return ts_cell_at(ts_world_cell(p, params.bin_min, params.inv_cell)); }\n";

/// A point's key is the block of 4³ world nodes holding its node
/// `floor((x - bin_min) * inv_cell)` in its tile's slot: slot · 8 + the block
/// (x + 2y + 4z) of the node's place in the tile, or [`block_keys`] − 1 when
/// the table does not hold the tile. Matter's P2G sorts by the block of each
/// point's stencil base node (bin_min +h/2).
const BLOCK_KEYS: &str = "@group(0) @binding(9) var<storage, read> ts_table: array<u32>;\n\
fn ts_pool_tiles() -> u32 { return params.pool_tiles; }\n\
fn ts_table_word(i: u32) -> u32 { return ts_table[i]; }\n\
fn sort_key_of(p: vec3<f32>) -> u32 {\n\
    let node = ts_world_cell(p, params.bin_min, params.inv_cell);\n\
    let slot = ts_slot_of(node >> vec3<u32>(3u));\n\
    if slot == WT_NONE { return 8u * params.pool_tiles; }\n\
    let b = (node & vec3<i32>(7)) >> vec3<u32>(2u);\n\
    return slot * 8u + u32(b.x + 2 * b.y + 4 * b.z);\n\
}\n";

/// Keys of a block sort over a pool of `pool_tiles` slots: 8 blocks a slot
/// and the one for points in tiles the table does not hold.
pub fn block_keys(pool_tiles: u32) -> u64 {
    8 * u64::from(pool_tiles) + 1
}

/// What a sorter keys points by.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum KeyRule {
    #[default]
    Dense,
    PooledCells,
    /// The pooled cells' keys, with points in tiles the table does not hold
    /// left out of the sort.
    PooledCellsHeld,
    PooledBlocks,
}

impl KeyRule {
    fn source(self) -> String {
        match self {
            Self::Dense => format!("{DENSE_KEYS}\n{SHADER}"),
            Self::PooledCells | Self::PooledCellsHeld => {
                format!("{READ_SIDE}\n{POOLED_KEYS}\n{SHADER}")
            }
            Self::PooledBlocks => format!("{READ_SIDE}\n{BLOCK_KEYS}\n{SHADER}"),
        }
    }

    /// Keys of a pooled rule over a pool of `pool_tiles` slots.
    fn pooled_keys(self, pool_tiles: u32) -> u64 {
        match self {
            Self::PooledBlocks => block_keys(pool_tiles),
            _ => pool_len(pool_tiles),
        }
    }
}
const ENTRIES: [&str; 6] = [
    "clear_counts",
    "count_particles",
    "write_ranges",
    "clear_tail",
    "scatter",
    "stabilise",
];

/// Live when the radius word is positive.
pub const LIVE_BY_RADIUS: u32 = 0;
/// Live when the id word is non-zero and the position finite.
pub const LIVE_BY_ID: u32 = 1;
/// Live when the kind word holds foam, bubble, spray or dust (4), excluding
/// [`WHITEWATER_EMPTY`], and the position is finite: dead particles count until
/// the tick's removal, as in FLIP.
pub const LIVE_BY_KIND: u32 = 2;
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
    pool_tiles: u32,
    /// The word of the count record (binding 10) holding the slots in use, or
    /// [`NO_COUNT_WORD`] when `count` is the slot count.
    count_word: u32,
    /// Padding where H was: a pooled sort's table carries its own.
    _pad_h: u32,
    /// 1 when a point whose tile the table does not hold is not counted.
    drop_absent: u32,
    /// Words of each slot's companion record; 0 when the job has none.
    companion_words: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

/// `SortParams::count_word` for a sort over a CPU slot count.
const NO_COUNT_WORD: u32 = 0xffff_ffff;
/// Bytes of a count record `[groups, 1, 1, n]`.
const RECORD_BYTES: u64 = 16;

/// Where the sort reads a record's position and liveness, in 4-byte words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordRead {
    pub stride_words: u32,
    pub position_word: u32,
    pub live_word: u32,
    pub live_rule: u32,
}

/// A liquid particle record: `position_radius` first, live when the radius is positive.
pub const LIQUID_PARTICLE_READ: RecordRead = RecordRead {
    stride_words: (std::mem::size_of::<FluidParticle>() / 4) as u32,
    position_word: 0,
    live_word: 3,
    live_rule: LIVE_BY_RADIUS,
};

/// Dispatch labels for a sort's six passes, so the profiler names its caller.
pub struct SortLabels {
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
    scan: ScanLabels {
        blocks: "node.sort_particles_into_cells.scan.blocks",
        add: "node.sort_particles_into_cells.scan.add",
    },
    ranges: "node.sort_particles_into_cells.ranges",
    tail: "node.sort_particles_into_cells.tail",
    scatter: "node.sort_particles_into_cells.scatter",
    stabilise: "node.sort_particles_into_cells.stabilise",
};

/// How many slots a sort covers.
#[derive(Clone, Copy)]
pub enum SortCount<'a> {
    /// The first `n` slots, a number the CPU has. At most every slot of every
    /// output is sorted.
    Slots(u32),
    /// The slots in use, on the GPU: `[groups, 1, 1, n]` at byte `offset` of
    /// `buffer` (a particle identity's record at `COUNT_OFFSET`, or a step's
    /// own). The count, scatter and stabilise passes dispatch from the
    /// triple, and `n` bounds the slots they read. Nothing reads a slot past
    /// `n`, so the tail pass that clears the outputs past the live total does
    /// not run: `sorted` and `order` keep their words there.
    Record { buffer: &'a GpuBuffer, offset: u64 },
}

/// One sort: the records, the bin grid, and the per-slot outputs to write.
pub struct SortJob<'a> {
    pub particles: &'a GpuBuffer,
    pub read: RecordRead,
    /// Records the particles buffer holds.
    pub capacity: u32,
    pub count: SortCount<'a>,
    /// Dense sort: the grid's minimum corner. Pooled sort: the lattice
    /// origin (`lattice_origin`), cells counted from it.
    pub bin_min: [f32; 3],
    pub inv_cell: f32,
    /// Dense sort: the grid's bins per axis. A pooled sort has no grid and
    /// does not read it.
    pub bins: [u32; 3],
    /// Liquid particle records only; `None` leaves it unwritten.
    pub sorted: Option<&'a GpuBuffer>,
    pub order: Option<&'a GpuBuffer>,
    /// A record beside each slot that moves with it; `None` moves none.
    pub companion: Option<Companion<'a>>,
    /// A GPU FLIP clock plan: an inactive slot's (live, no time to step)
    /// sort runs no pass and leaves every output as it was. `None` always sorts.
    pub gate: Option<&'a GpuBuffer>,
}

/// `words` u32s beside each slot of the particles, in `from`: the sort writes
/// slot i's to `to` where it puts slot i, and (a [`SortCount::Slots`] sort)
/// zeroes the slots past the live total, as it does `sorted`'s.
#[derive(Clone, Copy)]
pub struct Companion<'a> {
    pub from: &'a GpuBuffer,
    pub to: &'a GpuBuffer,
    pub words: u32,
}

manifold_core::testkit_visible! {
/// The counting sort itself, shared by `node.sort_particles_into_cells` and
/// the stage nodes that sort inside one dispatch chain (`node.gpu_flip_step`).
/// Owns the cell ranges and its scratch. Built for one key rule: dense bins
/// ([`ParticleSorter::prepare`], [`ParticleSorter::encode`]), the world
/// tiles' pool indices ([`ParticleSorter::prepare_pooled`], or
/// [`ParticleSorter::prepare_pooled_held`] to leave out points in tiles the
/// table does not hold) or their 4³ node blocks
/// ([`ParticleSorter::prepare_pooled_blocks`]), the pooled ones encoded by
/// [`ParticleSorter::encode_pooled`].
#[derive(Default)]
pub struct ParticleSorter {
    rule: KeyRule,
    pipelines: Vec<GpuComputePipeline>,
    scan: PrefixScan,
    rank: Option<GpuBuffer>,
    slot_input: Option<GpuBuffer>,
    ranges: Option<GpuBuffer>,
    /// Zeros, the gate an ungated sort binds.
    open: Option<GpuBuffer>,
}
}

impl ParticleSorter {
    /// Create the pipelines for dense bins. Call before any early return
    /// (compile contract).
    pub fn prepare(&mut self, device: &GpuDevice) {
        self.create_pipelines(device, KeyRule::Dense);
    }

    /// Create the pipelines for keys that are world-tile pool indices.
    pub fn prepare_pooled(&mut self, device: &GpuDevice) {
        self.create_pipelines(device, KeyRule::PooledCells);
    }

    /// Create the pipelines for keys that are world-tile pool indices, the
    /// canonical element's range always empty: a point in a tile the table
    /// does not hold is in no range and not in `sorted` or `order`.
    pub fn prepare_pooled_held(&mut self, device: &GpuDevice) {
        self.create_pipelines(device, KeyRule::PooledCellsHeld);
    }

    /// Create the pipelines for keys that are 4³ node blocks of the world
    /// tiles' slots ([`block_keys`] of them).
    pub fn prepare_pooled_blocks(&mut self, device: &GpuDevice) {
        self.create_pipelines(device, KeyRule::PooledBlocks);
    }

    fn create_pipelines(&mut self, device: &GpuDevice, rule: KeyRule) {
        if self.pipelines.is_empty() {
            self.rule = rule;
            let source = rule.source();
            for entry in ENTRIES {
                self.pipelines.push(device.create_compute_pipeline(
                    &source,
                    entry,
                    "node.sort_particles_into_cells",
                ));
            }
        }
        self.scan.prepare(device);
        if self.open.is_none() {
            self.open = Some(super::prefix_scan::open_gate(device));
        }
    }

    /// One range per key: per bin of the grid when dense, per element of a
    /// pooled field (`pool_len`) when pooled; at `key_storage_bytes` or more.
    pub fn ranges(&self) -> Option<&GpuBuffer> {
        self.ranges.as_ref()
    }

    /// Size the cell ranges for `bins` before anything reads them.
    pub fn reserve_ranges(&mut self, device: &GpuDevice, bins: [u32; 3]) -> Result<(), String> {
        self.reserve_keys(device, bin_total(bins))
    }

    /// Size the ranges for `keys` ranges before anything reads them.
    pub fn reserve_keys(&mut self, device: &GpuDevice, keys: u64) -> Result<(), String> {
        let range_bytes = key_storage_bytes(keys);
        if self
            .ranges
            .as_ref()
            .is_some_and(|ranges| ranges.size >= range_bytes)
        {
            return Ok(());
        }
        let buffer = manifold_node_engine::load::expand::admit_world_bytes(
            device.modifier_memory_snapshot(),
            range_bytes,
        )
        .map_err(|error| error.to_string())
        .and_then(|()| device.try_create_buffer(range_bytes, BufferUse::Upload))?;
        // Nothing reads a range the passes did not write as anything but empty.
        buffer.zero_fill();
        self.ranges = Some(buffer);
        Ok(())
    }

    /// Encode the six passes over dense bins. `prepare` and
    /// `reserve_ranges(job.bins)` first. Fails before any dispatch when the
    /// scan storage cannot be had.
    pub fn encode(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        job: &SortJob<'_>,
        labels: &SortLabels,
    ) -> Result<(), String> {
        if self.rule != KeyRule::Dense {
            return Err("a pooled sorter needs the slot table: use encode_pooled".into());
        }
        self.encode_keyed(device, encoder, job, None, labels)
    }

    /// Encode the six passes keyed by the world tiles: a particle's key is
    /// `ts_cell_at` of its world cell, `floor((x - job.bin_min) * job.inv_cell)`
    /// with no clamp, through `table` (the world-tile table of a pool of
    /// `pool_tiles` slots, `table_words(pool_tiles)` words). A particle whose
    /// tile has no slot lands in the canonical bucket, which no stored cell
    /// reads (or, after `prepare_pooled_held`, in no bucket).
    /// `prepare_pooled` or `prepare_pooled_held` and
    /// `reserve_keys(pool_len(pool_tiles))` first; or `prepare_pooled_blocks`
    /// and `reserve_keys(block_keys(pool_tiles))`, a point's key then its
    /// node's block.
    pub fn encode_pooled(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        job: &SortJob<'_>,
        table: &GpuBuffer,
        pool_tiles: u32,
        labels: &SortLabels,
    ) -> Result<(), String> {
        if self.rule == KeyRule::Dense {
            return Err("a dense sorter has no slot table: use encode".into());
        }
        self.encode_keyed(device, encoder, job, Some((table, pool_tiles)), labels)
    }

    fn encode_keyed(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        job: &SortJob<'_>,
        pool: Option<(&GpuBuffer, u32)>,
        labels: &SortLabels,
    ) -> Result<(), String> {
        let keys = match pool {
            Some((table, pool_tiles)) => {
                if table.size < table_words(pool_tiles) * 4 {
                    return Err("the table holds fewer words than its pool's".into());
                }
                self.rule.pooled_keys(pool_tiles)
            }
            None => bin_total(job.bins),
        };
        let bin_total = keys as u32;
        let cell_counts = self.scan.buffer(device, bin_total as usize)?.clone();
        let particle_size = std::mem::size_of::<FluidParticle>() as u64;
        let sorted_capacity = [
            job.sorted.map(|b| b.size / particle_size),
            job.order.map(|b| b.size / 4),
            job.companion
                .map(|c| c.to.size / (u64::from(c.words.max(1)) * 4)),
        ]
        .into_iter()
        .flatten()
        .fold(u64::from(job.capacity), u64::min) as u32;
        let rank_bytes = u64::from(job.capacity.max(1)) * 4;
        if self.rank.as_ref().is_none_or(|rank| rank.size < rank_bytes) {
            self.rank = Some(device.create_buffer(rank_bytes, BufferUse::Gpu));
        }
        if self
            .slot_input
            .as_ref()
            .is_none_or(|slots| slots.size < rank_bytes)
        {
            self.slot_input = Some(device.create_buffer(rank_bytes, BufferUse::Gpu));
        }
        let ranges = self
            .ranges
            .as_ref()
            .ok_or("the cell ranges were not reserved")?;
        if ranges.size < key_storage_bytes(keys) {
            return Err("the cell ranges were reserved for fewer keys".into());
        }
        let gate = job
            .gate
            .or(self.open.as_ref())
            .expect("sort pipelines prepared");
        // The slots the passes cover: a CPU number, or an upper bound with the
        // GPU's record bound to the shader (the gate stands in when unused).
        let (count, count_word, counts) = match job.count {
            SortCount::Slots(n) => (n.min(sorted_capacity), NO_COUNT_WORD, gate),
            SortCount::Record { buffer, offset } => {
                if !offset.is_multiple_of(4) || buffer.size < offset + RECORD_BYTES {
                    return Err("the count record is smaller than its four words".into());
                }
                (sorted_capacity, (offset / 4 + 3) as u32, buffer)
            }
        };
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
            pool_tiles: pool.map_or(0, |(_, pool_tiles)| pool_tiles),
            count_word,
            _pad_h: 0,
            drop_absent: u32::from(self.rule == KeyRule::PooledCellsHeld),
            companion_words: job.companion.map_or(0, |c| c.words),
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let mut bindings = vec![
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: job.particles,
                offset: 0,
            },
            // Unwritten outputs (write flag 0) bind rank to keep the layout.
            GpuBinding::Buffer {
                binding: 2,
                buffer: job.sorted.unwrap_or(rank),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: ranges,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &cell_counts,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: rank,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: job.order.unwrap_or(rank),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 7,
                buffer: slot_input,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 8,
                buffer: gate,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 10,
                buffer: counts,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 11,
                buffer: job.companion.map_or(job.particles, |c| c.from),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 12,
                buffer: job.companion.map_or(rank, |c| c.to),
                offset: 0,
            },
        ];
        if let Some((table, _)) = pool {
            bindings.push(GpuBinding::Buffer {
                binding: 9,
                buffer: table,
                offset: 0,
            });
        }
        let groups = |n: u32| [n.div_ceil(256).max(1), 1, 1];
        // One thread per slot in use: direct over the CPU's count, indirect
        // from the GPU's record.
        let over_slots =
            |encoder: &mut GpuEncoder, pipeline: &GpuComputePipeline, label| match job.count {
                SortCount::Slots(_) => {
                    encoder.dispatch_compute(pipeline, &bindings, groups(count), label);
                }
                SortCount::Record { buffer, offset } => {
                    encoder.dispatch_compute_indirect(pipeline, &bindings, buffer, offset, label);
                }
            };
        let [
            clear,
            count_pass,
            write_ranges,
            clear_tail,
            scatter,
            stabilise,
        ] = &self.pipelines[..]
        else {
            unreachable!("six sort pipelines");
        };
        encoder.dispatch_compute(clear, &bindings, groups(bin_total), labels.clear);
        encoder.compute_memory_barrier_buffers();
        over_slots(encoder, count_pass, labels.count);
        encoder.compute_memory_barrier_buffers();
        match job.gate {
            Some(gate) => {
                self.scan
                    .encode_labelled_gated(encoder, bin_total as usize, labels.scan, gate)
            }
            None => self
                .scan
                .encode_labelled(encoder, bin_total as usize, labels.scan),
        }
        encoder.dispatch_compute(write_ranges, &bindings, groups(bin_total), labels.ranges);
        if matches!(job.count, SortCount::Slots(_)) {
            encoder.dispatch_compute(clear_tail, &bindings, groups(sorted_capacity), labels.tail);
        }
        encoder.compute_memory_barrier_buffers();
        over_slots(encoder, scatter, labels.scatter);
        encoder.compute_memory_barrier_buffers();
        // One thread per scattered slot; there are at most `count`.
        over_slots(encoder, stabilise, labels.stabilise);
        Ok(())
    }
}

/// How the sort reads a whitewater pool slot.
pub fn whitewater_record_read() -> RecordRead {
    record_read(&ArrayType::of_known::<crate::whitewater::WhitewaterParticle>())
        .expect("a whitewater slot carries a position and a kind")
}

pub fn record_read(layout: &ArrayType) -> Option<RecordRead> {
    let stride_words = layout.item_size / 4;
    if let Some((offset, ChannelElementType::Vec4F)) =
        std430_channel(layout.specs, well_known::POSITION_RADIUS)
    {
        return Some(RecordRead {
            stride_words,
            position_word: offset / 4,
            live_word: offset / 4 + 3,
            live_rule: LIVE_BY_RADIUS,
        });
    }
    if let (Some((position, ChannelElementType::Vec4F)), Some((kind, ChannelElementType::U32))) = (
        std430_channel(layout.specs, well_known::POSITION_LIFETIME),
        std430_channel(layout.specs, well_known::KIND),
    ) {
        return Some(RecordRead {
            stride_words,
            position_word: position / 4,
            live_word: kind / 4,
            live_rule: LIVE_BY_KIND,
        });
    }
    match (
        std430_channel(layout.specs, well_known::POSITION),
        std430_channel(layout.specs, well_known::ID),
    ) {
        (Some((position, ChannelElementType::Vec3F)), Some((id, ChannelElementType::U32))) => {
            Some(RecordRead {
                stride_words,
                position_word: position / 4,
                live_word: id / 4,
                live_rule: LIVE_BY_ID,
            })
        }
        _ => None,
    }
}

#[macro_export]
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

/// An Int param: whole numbers in `[min, max]`, stored as a float.
#[macro_export]
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
pub use crate::int_param;

manifold_node_engine::primitive! {
    name: SortParticlesIntoCells,
    type_id: "node.sort_particles_into_cells",
    purpose: "Sort particles into the cells of a world-tile pool, so neighbour searches read only nearby cells. Outputs the particles in cell order (inactive records past the live total), each cell's start and count, and each sorted slot's input index. Cells are cell_size metres, cell c centred on c·cell_size, so a point's cell is floor(x / cell_size + 0.5) per axis; cell_ranges holds one range per pool element (pool_len of pool_tiles), the range at the pool index of cell c being the particles in c. Within a cell, particles keep their input order, so every output is the same on every run. A particle in a tile the table does not hold is dropped: it is in no range and not in sorted or order, so the canonical element and free slots are empty ranges. `order` gives each sorted slot's input index (0xffffffff past the live total), for consumers that keep their own per-particle arrays in input order. Either `sorted` or `order` may be left unwired. With `enabled` 0 it does nothing and every output keeps its contents. Particles may be liquid particle records (live when the radius is positive) or any record with a position and an id (live when the id is non-zero and the position finite), such as matter points.",
    inputs: {
        particles: Channels[permissive] required,
        count: ScalarF32 optional,
        enabled: ScalarF32 optional,
        tiles: Array(u32) required,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
    },
    outputs: {
        sorted: Array(FluidParticle),
        cell_ranges: Array(CellRange),
        order: Array(u32),
    },
    params: [
        float_param!("enabled", "Enabled", 1.0, 0.0, 1.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire count from the producer's live count so stale records past it are never sorted; records with radius 0 are skipped. tiles, pool_tiles and cell_size come from the world tiles the cells live on (node.surface_tiles for the display), and every atom that searches these cells indexes cell_ranges with ts_cell_at on the same tiles. Matter points wire straight in from node.matter_state's out; `sorted` holds liquid particle records, so leave it unwired and use `order` when sorting anything else. A pool the device cannot hold ranges for is a named error. Inside a substep region, gate it with the boundary's tick_end so it sorts once per tick.",
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

/// Bytes of range storage for `keys` keys, at least one range so it binds.
pub fn key_storage_bytes(keys: u64) -> u64 {
    keys.max(1) * std::mem::size_of::<CellRange>() as u64
}

impl Primitive for SortParticlesIntoCells {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "cell_ranges"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "cell_ranges")
            .then_some(self.sorter.ranges())
            .flatten()
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        match port {
            "sorted" | "order" => inputs
                .iter()
                .find(|(name, _)| *name == "particles")
                .map(|&(_, n)| n),
            // Provided storage: a one-record hint, sized to the pool at run time.
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
        self.sorter.prepare_pooled_held(ctx.gpu_encoder().device);
        if ctx.scalar_or_param("enabled", 1.0) <= 0.5 {
            return;
        }
        if requested.is_some_and(|count| !count.is_finite()) {
            ctx.error("Sort Particles Into Cells: count must be finite");
            return;
        }
        let pool = match tile_pool(ctx, "Sort Particles Into Cells") {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        // The ranges are sized from the same pool every pass dispatches over,
        // before any of them is encoded.
        if let Err(error) = self
            .sorter
            .reserve_keys(ctx.gpu_encoder().device, pool.elements())
        {
            ctx.error(format!(
                "Sort Particles Into Cells: a pool of {} tiles needs {} bytes of cell ranges the device cannot give: {error}",
                pool.pool_tiles,
                key_storage_bytes(pool.elements())
            ));
            return;
        }
        let (Some(particles), Some(tiles)) =
            (ctx.inputs.array("particles"), ctx.inputs.array("tiles"))
        else {
            return;
        };
        let Some(layout) = ctx.inputs.array_layout("particles") else {
            ctx.error("Sort Particles Into Cells: the particles wire carries no record layout");
            return;
        };
        let Some(read) = record_read(&layout) else {
            ctx.error("Sort Particles Into Cells: particles need a position_radius channel, or position and id channels");
            return;
        };
        if ctx.outputs.array("sorted").is_some() && layout.specs != FLUID_PARTICLE_SPECS {
            ctx.error("Sort Particles Into Cells: sorted holds liquid particle records; leave it unwired and use order for these points");
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
            count: SortCount::Slots(count),
            bin_min: lattice_origin(pool.cell_size),
            inv_cell: 1.0 / pool.cell_size,
            bins: [0; 3],
            sorted,
            order,
            companion: None,
            gate: None,
        };
        let gpu = ctx.gpu_encoder();
        let encoded = self.sorter.encode_pooled(
            gpu.device,
            gpu.native_enc,
            &job,
            tiles,
            pool.pool_tiles,
            &NODE_LABELS,
        );
        if let Err(error) = encoded {
            ctx.error(format!("Sort Particles Into Cells: {error}"));
        }
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_rule_composes_a_valid_sort() {
        for rule in [
            KeyRule::Dense,
            KeyRule::PooledCells,
            KeyRule::PooledCellsHeld,
            KeyRule::PooledBlocks,
        ] {
            let source = rule.source();
            let module = naga::front::wgsl::parse_str(&source)
                .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        }
        assert_eq!(block_keys(3), 25);
    }
}
