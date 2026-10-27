//! World tiles, the build (WATER_INFINITE_DOMAIN_DESIGN.md section 2): the
//! hash table keyed by world tile that every pooled lattice field is read
//! through, rebuilt on the GPU every active step from a set of points
//! (`world_tiles.wgsl`). The read side, the sizes and the table layout live in
//! `manifold_node_engine::scene::world_tiles`.
//!
//! A table is built from particles ([`WorldTiles::new`],
//! [`WorldTiles::encode_allocation`]): each particle's tile and every tile
//! within `ring_max` of one join the table, held slots whose tile stays
//! within `alloc_ring` keep their slot, and the rest of the wanted tiles get
//! slots from the free stack, ring <= 1 first. Or from the level below
//! ([`WorldTiles::new_parents`], [`WorldTiles::encode_parents`]): a coarse
//! level's tiles are the parents of the level below's held tiles, handed
//! out in table order from scratch every build, with a row of 8 children per
//! slot.
//!
//! The particle read is a composed WGSL snippet ([`LIQUID_POINTS`] for
//! liquid particle records), as the sort composes its key: it binds the
//! records at [`POINTS_BINDING`] and defines `wt_point_live(i)` and
//! `wt_point_cell(i)`.
//!
//! Every slot number is the same on every run whatever order GPU threads
//! arrive in (ordered linear probing, frees in slot order, handout in table
//! order through a prefix scan); list order is free, so readers index their
//! partials by slot.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::scene::world_tiles::{
    EMPTY, NONE, READ_SIDE, TILE, table_entries, table_words,
};

use crate::primitives::prefix_scan::{PrefixScan, ScanLabels, storage_words};

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod mirror;

/// The build passes. Composed after [`READ_SIDE`] and a points snippet.
pub const SHADER: &str = include_str!("primitives/shaders/world_tiles.wgsl");

/// Points from liquid particle records: live when the radius is positive.
pub const LIQUID_POINTS: &str = include_str!("primitives/shaders/world_tiles_liquid.wgsl");

/// Points from the level below's held slots, and the children rows pass.
pub const PARENT_POINTS: &str = include_str!("primitives/shaders/world_tiles_parents.wgsl");

/// Where a points snippet binds its records.
pub const POINTS_BINDING: u32 = 13;

/// Where a parents build writes its children rows.
const CHILD_ROWS_BINDING: u32 = 14;

/// Where a particle build binds its slot record (`world_tiles_liquid.wgsl`).
const RECORD_BINDING: u32 = 15;

// LIQUID_POINTS reads a record as two vec4s.
const _: () = assert!(std::mem::size_of::<FluidParticle>() == 32);

/// Exhausted bit: an entry wanting a slot found the free stack empty.
pub const POOL_SHORT: u32 = 1;
/// Exhausted bit: a key was dropped, by the dilation's H/2 guard or from a
/// full table.
pub const TABLE_FULL: u32 = 2;
/// Exhausted bit: two live tiles a key span apart share a key.
pub const ALIAS: u32 = 4;

/// How far a particle build reaches, fixed for a table's life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reach {
    /// Tiles within this Chebyshev tile distance of a particle's tile join
    /// the table.
    pub ring_max: u32,
    /// Tiles past this ring get no slot (at most `ring_max`).
    pub alloc_ring: u32,
    /// The cell set C's reach from a particle's cell, in cells (at most a
    /// tile, so C lies in ring <= 1).
    pub cell_reach: u32,
}

/// Word of the counts holding the held slots with rank <= `rank` (word 0
/// is |C|), for `rank` up to `ring_max + 1`.
pub fn rank_count_word(rank: u32) -> usize {
    rank as usize
}

/// Word of the counts holding the retired list's length.
pub fn retired_word(ring_max: u32) -> usize {
    ring_max as usize + 2
}

/// Word of the counts holding the free stack's live length.
pub fn free_top_word(ring_max: u32) -> usize {
    ring_max as usize + 3
}

/// Word of the counts holding the exhausted bits ([`POOL_SHORT`],
/// [`TABLE_FULL`], [`ALIAS`]), rewritten every active build.
pub fn exhausted_word(ring_max: u32) -> usize {
    ring_max as usize + 4
}

/// Word of the counts holding the fresh list's length.
pub fn fresh_word(ring_max: u32) -> usize {
    ring_max as usize + 5
}

/// Words of the counts.
pub fn count_words(ring_max: u32) -> u64 {
    u64::from(ring_max) + 6
}

/// Byte offset in the args of the triple over the slots with rank <=
/// `rank`, for `rank` up to `ring_max`.
pub fn list_args_offset(rank: u32) -> u64 {
    12 * u64::from(rank)
}

/// Byte offset of the triple over every held slot (rank <= ring_max, which
/// is every rank a held slot has when `ring_max` >= 1).
pub fn allocated_args_offset(ring_max: u32) -> u64 {
    list_args_offset(ring_max)
}

/// Byte offset of the retired list's triple.
pub fn retired_args_offset(ring_max: u32) -> u64 {
    12 * (u64::from(ring_max) + 1)
}

/// Index of the 8-byte replay range after the triples: (0, the build's
/// `replay_count`) on an active step, (0, 0) on an inactive one.
pub fn replay_range_index(ring_max: u32) -> u32 {
    (3 * (ring_max + 2)).div_ceil(2)
}

/// Byte offset of the fresh list's triple, after the replay range.
pub fn fresh_args_offset(ring_max: u32) -> u64 {
    8 * (u64::from(replay_range_index(ring_max)) + 1)
}

/// Words of the args: the list and retired triples, the replay range, the
/// fresh triple.
pub fn args_words(ring_max: u32) -> u64 {
    2 * u64::from(replay_range_index(ring_max)) + 5
}

/// Words of the build's own counters: the rank histogram and the list
/// cursors per rank, the keys held, the seeds, the retired and fresh cursors.
fn build_words(ring_max: u32) -> u64 {
    2 * (u64::from(ring_max) + 2) + 4
}

/// Bytes a particle build ([`WorldTiles::new`]) holds for a pool of
/// `pool_tiles` slots at `ring_max`: the table, counts, triples and lists,
/// and the build's scratch and scan. For a memory check before the table
/// exists; [`WorldTiles::bytes`] reads the buffers themselves.
pub fn held_bytes(pool_tiles: u32, ring_max: u32) -> u64 {
    let entries = table_entries(pool_tiles);
    let p = u64::from(pool_tiles);
    let words = table_words(pool_tiles)
        + count_words(ring_max)
        + args_words(ring_max)
        + 3 * p
        + 9 * u64::from(entries)
        + 3 * p
        + build_words(ring_max)
        + 3
        + storage_words(scan_len(pool_tiles, entries)) as u64;
    words * 4
}

/// Children a parents table's slot rows hold: the 2³ tiles below a tile.
const CHILDREN_PER_SLOT: u64 = 8;

/// Bytes a parents build ([`WorldTiles::new_parents`]) holds for a pool of
/// `pool_tiles` slots: a particle build's at ring 0 and the children rows.
pub fn parents_bytes(pool_tiles: u32) -> u64 {
    held_bytes(pool_tiles, 0) + CHILDREN_PER_SLOT * u64::from(pool_tiles) * 4
}

/// Values the build's prefix scan covers: freed flags per slot, then the
/// two want sections per entry.
fn scan_len(pool_tiles: u32, entries: u32) -> usize {
    pool_tiles as usize + 2 * entries as usize
}

const SCAN_LABELS: ScanLabels = ScanLabels {
    blocks: "world_tiles.scan.blocks",
    add: "world_tiles.scan.add",
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BuildParams {
    origin: [f32; 3],
    inv_cell: f32,
    pool_tiles: u32,
    entries: u32,
    ring_max: u32,
    alloc_ring: u32,
    cell_reach: u32,
    point_count: u32,
    from_scratch: u32,
    replay_count: u32,
    child_entries: u32,
    child_pool_tiles: u32,
    _pad: [u32; 2],
}

/// The table's slot lists, `P` words each.
pub struct TileLists {
    /// Held slots by rank: rank 0 (the cell set C) first, then rank by rank;
    /// the counts give each rank's end.
    pub by_rank: GpuBuffer,
    /// Slots carried from C last build into a rank past 0.
    pub retired: GpuBuffer,
    /// Slots handed out this build.
    pub fresh: GpuBuffer,
}

struct Pipelines {
    clear: GpuComputePipeline,
    seed: GpuComputePipeline,
    seeds: GpuComputePipeline,
    dilate: GpuComputePipeline,
    rings: GpuComputePipeline,
    near: GpuComputePipeline,
    carry: GpuComputePipeline,
    wants: GpuComputePipeline,
    free: GpuComputePipeline,
    allocate: GpuComputePipeline,
    slots: GpuComputePipeline,
    finish: GpuComputePipeline,
    children: Option<GpuComputePipeline>,
}

/// One particle build's inputs.
pub struct Allocation<'a> {
    /// The records the points snippet reads.
    pub points: &'a GpuBuffer,
    /// The slots in use, `[groups, 1, 1, n]` (the GPU FLIP step's slot
    /// record): the point passes dispatch from its triple and read records
    /// [0, n) of `points`, so records past n are never binned.
    pub record: &'a GpuBuffer,
    /// The lattice origin (`lattice_origin`) and 1 / cell size.
    pub origin: [f32; 3],
    pub inv_cell: f32,
    /// Written into the args' replay range on an active step.
    pub replay_count: u32,
    /// A GPU FLIP clock plan: an inactive slot builds nothing and keeps the
    /// table.
    pub gate: &'a GpuBuffer,
}

/// A world tile table with its lists, counts, triples and build scratch,
/// for a pool of `P` slots.
pub struct WorldTiles {
    reach: Reach,
    pool_tiles: u32,
    entries: u32,
    points: &'static str,
    /// [`table_words`]: keys, entry slots, slot rows, neighbour rows.
    table: GpuBuffer,
    /// [`count_words`].
    counts: GpuBuffer,
    /// [`args_words`].
    args: GpuBuffer,
    lists: TileLists,
    /// Per entry: ring, near, tile x, y, z, the seed pass's tile x, y, z;
    /// then the seed list (9H words).
    entry_scratch: GpuBuffer,
    /// Per slot: state, rank, then the free stack (3P words).
    slot_scratch: GpuBuffer,
    build_words: GpuBuffer,
    /// The dilation's indirect triple.
    build_args: GpuBuffer,
    /// A parents table's 8 children per slot.
    child_rows: Option<GpuBuffer>,
    scan: PrefixScan,
    pipelines: Option<Pipelines>,
}

fn admit(device: &GpuDevice, bytes: u64) -> Result<GpuBuffer, String> {
    manifold_node_engine::load::expand::admit_candidate_bytes(
        device.modifier_memory_snapshot(),
        bytes,
    )
    .map_err(|error| error.to_string())?;
    device.try_create_buffer_shared(bytes)
}

/// A shared buffer holding `words`, written before any GPU work sees it.
fn words_buffer(device: &GpuDevice, words: &[u32]) -> Result<GpuBuffer, String> {
    let buffer = admit(device, (words.len() * 4) as u64)?;
    // SAFETY: a new shared buffer sized for `words`; no GPU work has touched it.
    unsafe { buffer.write(0, bytemuck::cast_slice(words)) };
    Ok(buffer)
}

fn zeroed(device: &GpuDevice, words: u64) -> Result<GpuBuffer, String> {
    let buffer = admit(device, words * 4)?;
    buffer.zero_fill();
    Ok(buffer)
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

fn groups(threads: u64) -> [u32; 3] {
    [threads.div_ceil(256).max(1) as u32, 1, 1]
}

impl WorldTiles {
    /// A table built from particles read by the `points` snippet. Every
    /// slot free, every key empty: the first active build hands out the
    /// whole pool.
    pub fn new(
        device: &GpuDevice,
        pool_tiles: u32,
        reach: Reach,
        points: &'static str,
    ) -> Result<Self, String> {
        if reach.alloc_ring > reach.ring_max {
            return Err(format!(
                "world tiles: alloc ring {} is past ring max {}",
                reach.alloc_ring, reach.ring_max
            ));
        }
        if reach.cell_reach > TILE {
            return Err(format!(
                "world tiles: a cell reach of {} is more than a tile",
                reach.cell_reach
            ));
        }
        Self::with_points(device, pool_tiles, reach, points, false)
    }

    /// A coarse level's table, built from the level below
    /// ([`Self::encode_parents`]).
    pub fn new_parents(device: &GpuDevice, pool_tiles: u32) -> Result<Self, String> {
        let reach = Reach {
            ring_max: 0,
            alloc_ring: 0,
            cell_reach: 0,
        };
        Self::with_points(device, pool_tiles, reach, PARENT_POINTS, true)
    }

    fn with_points(
        device: &GpuDevice,
        pool_tiles: u32,
        reach: Reach,
        points: &'static str,
        parents: bool,
    ) -> Result<Self, String> {
        if pool_tiles == 0 {
            return Err("world tiles: a pool needs at least one tile".into());
        }
        let entries = table_entries(pool_tiles);
        let p = pool_tiles as usize;
        let h = entries as usize;
        let mut table = vec![NONE; table_words(pool_tiles) as usize];
        table[..h].fill(EMPTY);
        let mut counts = vec![0; count_words(reach.ring_max) as usize];
        counts[free_top_word(reach.ring_max)] = pool_tiles;
        // State and rank zero; the free stack every slot, slot 0 on top.
        let mut slots = vec![0; 3 * p];
        for (i, word) in slots[2 * p..].iter_mut().enumerate() {
            *word = pool_tiles - 1 - i as u32;
        }
        let mut scan = PrefixScan::default();
        scan.buffer(device, scan_len(pool_tiles, entries))?;
        let child_rows = if parents {
            Some(words_buffer(
                device,
                &vec![NONE; CHILDREN_PER_SLOT as usize * p],
            )?)
        } else {
            None
        };
        Ok(Self {
            reach,
            pool_tiles,
            entries,
            points,
            table: words_buffer(device, &table)?,
            counts: words_buffer(device, &counts)?,
            args: zeroed(device, args_words(reach.ring_max))?,
            lists: TileLists {
                by_rank: zeroed(device, u64::from(pool_tiles))?,
                retired: zeroed(device, u64::from(pool_tiles))?,
                fresh: zeroed(device, u64::from(pool_tiles))?,
            },
            entry_scratch: zeroed(device, 9 * u64::from(entries))?,
            slot_scratch: words_buffer(device, &slots)?,
            build_words: zeroed(device, build_words(reach.ring_max))?,
            build_args: words_buffer(device, &[0, 1, 1])?,
            child_rows,
            scan,
            pipelines: None,
        })
    }

    /// Create the pipelines. Call before any early return (compile contract).
    pub fn prepare(&mut self, device: &GpuDevice) {
        self.scan.prepare(device);
        if self.pipelines.is_some() {
            return;
        }
        let source = format!("{READ_SIDE}\n{}\n{SHADER}", self.points);
        let make = |entry: &str| device.create_compute_pipeline(&source, entry, "world_tiles");
        self.pipelines = Some(Pipelines {
            clear: make("wt_clear"),
            seed: make("wt_seed"),
            seeds: make("wt_seeds"),
            dilate: make("wt_dilate"),
            rings: make("wt_rings"),
            near: make("wt_near"),
            carry: make("wt_carry"),
            wants: make("wt_wants"),
            free: make("wt_free"),
            allocate: make("wt_allocate"),
            slots: make("wt_slots"),
            finish: make("wt_finish"),
            children: self.child_rows.is_some().then(|| make("wt_children")),
        });
    }

    /// Build the table from `job.count` points: their tiles and every tile
    /// within `ring_max`, rings and nearness, the carry, the free stack and
    /// the handout, then the slot rows, neighbour rows, lists, counts and
    /// triples. `prepare` first.
    pub fn encode_allocation(&self, enc: &mut GpuEncoder, job: &Allocation<'_>) {
        debug_assert!(
            self.child_rows.is_none(),
            "a parents table builds through encode_parents"
        );
        let params = BuildParams {
            origin: job.origin,
            inv_cell: job.inv_cell,
            pool_tiles: self.pool_tiles,
            entries: self.entries,
            ring_max: self.reach.ring_max,
            alloc_ring: self.reach.alloc_ring,
            cell_reach: self.reach.cell_reach,
            point_count: 0,
            from_scratch: 0,
            replay_count: job.replay_count,
            child_entries: 0,
            child_pool_tiles: 0,
            _pad: [0; 2],
        };
        self.encode_build(enc, &params, job.points, Some(job.record), job.gate);
    }

    /// Build a coarse level from `child`, the level below, encoded earlier
    /// in this command buffer: the parents of its held tiles, slots in table
    /// order, and each slot's children row. `prepare` first.
    pub fn encode_parents(&self, enc: &mut GpuEncoder, child: &WorldTiles, gate: &GpuBuffer) {
        debug_assert!(
            self.child_rows.is_some(),
            "a particle table builds through encode_allocation"
        );
        let params = BuildParams {
            origin: [0.0; 3],
            inv_cell: 0.0,
            pool_tiles: self.pool_tiles,
            entries: self.entries,
            ring_max: 0,
            alloc_ring: 0,
            cell_reach: 0,
            point_count: child.pool_tiles,
            from_scratch: 1,
            replay_count: 0,
            child_entries: child.entries,
            child_pool_tiles: child.pool_tiles,
            _pad: [0; 2],
        };
        self.encode_build(enc, &params, &child.table, None, gate);
    }

    fn encode_build(
        &self,
        enc: &mut GpuEncoder,
        params: &BuildParams,
        points: &GpuBuffer,
        record: Option<&GpuBuffer>,
        gate: &GpuBuffer,
    ) {
        let pipes = self.pipelines.as_ref().expect("world tiles prepared");
        let fixed = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(params),
            },
            buffer(1, &self.table),
            buffer(2, &self.counts),
            buffer(3, &self.args),
            buffer(4, &self.lists.by_rank),
            buffer(5, &self.lists.retired),
            buffer(6, &self.lists.fresh),
            buffer(7, &self.entry_scratch),
            buffer(8, &self.slot_scratch),
            buffer(9, &self.build_words),
            buffer(10, &self.build_args),
            buffer(11, self.scan.buffer_ref()),
            buffer(12, gate),
            buffer(POINTS_BINDING, points),
        ];
        // Room after the fixed bindings for the slot record (a particle
        // build) or the children rows (the parents' last pass).
        let mut bound = [fixed[0]; 15];
        bound[..14].copy_from_slice(&fixed);
        let mut len = 14;
        if let Some(record) = record {
            bound[len] = buffer(RECORD_BINDING, record);
            len += 1;
        }
        let bindings = &bound[..len];
        let per_entry = groups(u64::from(self.entries));
        let per_slot = groups(u64::from(self.pool_tiles));
        let pass = |enc: &mut GpuEncoder, pipeline: &GpuComputePipeline, grid, label| {
            enc.dispatch_compute(pipeline, bindings, grid, label);
            enc.compute_memory_barrier_buffers();
        };
        // A particle build's point passes cover the record's slots in use; a
        // parents build's every slot of the level below.
        let per_point = |enc: &mut GpuEncoder, pipeline: &GpuComputePipeline, label| {
            match record {
                Some(record) => enc.dispatch_compute_indirect(pipeline, bindings, record, 0, label),
                None => enc.dispatch_compute(
                    pipeline,
                    bindings,
                    groups(u64::from(params.point_count)),
                    label,
                ),
            }
            enc.compute_memory_barrier_buffers();
        };
        pass(enc, &pipes.clear, per_entry, "world_tiles.clear");
        per_point(enc, &pipes.seed, "world_tiles.seed");
        per_point(enc, &pipes.seeds, "world_tiles.seeds");
        if self.reach.ring_max > 0 {
            for (pipeline, label) in [
                (&pipes.dilate, "world_tiles.dilate"),
                (&pipes.rings, "world_tiles.rings"),
            ] {
                enc.dispatch_compute_indirect(pipeline, bindings, &self.build_args, 0, label);
                enc.compute_memory_barrier_buffers();
            }
        }
        per_point(enc, &pipes.near, "world_tiles.near");
        pass(enc, &pipes.carry, per_slot, "world_tiles.carry");
        pass(enc, &pipes.wants, per_entry, "world_tiles.wants");
        self.scan.encode_labelled_gated(
            enc,
            scan_len(self.pool_tiles, self.entries),
            SCAN_LABELS,
            gate,
        );
        pass(enc, &pipes.free, per_slot, "world_tiles.free");
        pass(enc, &pipes.allocate, per_entry, "world_tiles.allocate");
        pass(enc, &pipes.slots, per_slot, "world_tiles.slots");
        if let (Some(children), Some(rows)) = (&pipes.children, &self.child_rows) {
            let mut with_rows = bound;
            with_rows[14] = buffer(CHILD_ROWS_BINDING, rows);
            enc.dispatch_compute(children, &with_rows[..15], per_slot, "world_tiles.children");
            enc.compute_memory_barrier_buffers();
        }
        pass(enc, &pipes.finish, [1, 1, 1], "world_tiles.finish");
    }

    /// The table ([`table_words`] words), published to every reader.
    pub fn table(&self) -> &GpuBuffer {
        &self.table
    }

    pub fn lists(&self) -> &TileLists {
        &self.lists
    }

    /// The indirect triples ([`args_words`] words).
    pub fn args(&self) -> &GpuBuffer {
        &self.args
    }

    /// The counts ([`count_words`] words).
    pub fn counts(&self) -> &GpuBuffer {
        &self.counts
    }

    /// A parents table's children rows: 8 slots of the level below per slot.
    pub fn child_rows(&self) -> Option<&GpuBuffer> {
        self.child_rows.as_ref()
    }

    /// P.
    pub fn pool_tiles(&self) -> u32 {
        self.pool_tiles
    }

    /// H.
    pub fn table_entries(&self) -> u32 {
        self.entries
    }

    pub fn reach(&self) -> Reach {
        self.reach
    }

    /// Bytes of GPU memory the table, its lists and its build scratch hold.
    pub fn bytes(&self) -> u64 {
        [
            &self.table,
            &self.counts,
            &self.args,
            &self.lists.by_rank,
            &self.lists.retired,
            &self.lists.fresh,
            &self.entry_scratch,
            &self.slot_scratch,
            &self.build_words,
            &self.build_args,
            self.scan.buffer_ref(),
        ]
        .iter()
        .map(|buffer| buffer.size)
        .sum::<u64>()
            + self.child_rows.as_ref().map_or(0, |rows| rows.size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(points: &str) {
        let shader = format!("{READ_SIDE}\n{points}\n{SHADER}");
        let module = naga::front::wgsl::parse_str(&shader).expect("world_tiles.wgsl parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("world_tiles.wgsl validates");
    }

    #[test]
    fn build_shader_validates_with_both_point_sources() {
        validate(LIQUID_POINTS);
        validate(PARENT_POINTS);
    }

    /// The args keep every triple 4-byte aligned before an 8-byte aligned
    /// replay range, the fresh triple after it, for the ring reaches the
    /// solvers use.
    #[test]
    fn args_layout_holds_every_triple_apart() {
        for ring_max in 0..5 {
            let range = 8 * u64::from(replay_range_index(ring_max));
            assert!(
                retired_args_offset(ring_max) + 12 <= range,
                "ring_max {ring_max}"
            );
            assert_eq!(
                allocated_args_offset(ring_max) + 12,
                retired_args_offset(ring_max)
            );
            assert_eq!(fresh_args_offset(ring_max), range + 8);
            assert_eq!(fresh_args_offset(ring_max) + 12, 4 * args_words(ring_max));
            assert!(free_top_word(ring_max) < count_words(ring_max) as usize);
            assert_eq!(fresh_word(ring_max) + 1, count_words(ring_max) as usize);
        }
    }
}
