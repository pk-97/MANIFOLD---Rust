//! The step's tile table (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3): its sizes, storage and per-step build.

use super::*;
use manifold_water_liquid::tile_storage::{TILE, tile_total};

/// The table's word for no slot and no tile (`TS_NONE`).
const TS_NONE: u32 = u32::MAX;

/// The cell passes' reach from a particle-holding cell, in cells: the tile
/// set C is every tile within it (`tiles_classify` writes the distance).
/// The shader holds its own copy; the proofs' CPU model reads this one.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub const CELL_REACH: u32 = 2;

manifold_core::testkit_visible! {
/// The farthest tile ring any extend layer reaches at `band` layers: a
/// layer fills faces `1 + layer` cells from the water. A per-step constant,
/// not a cap: the table is sized for it.
pub(crate) fn ring_max(band: u32) -> u32 {
    (1 + band).div_ceil(TILE)
}
}

/// Tiles with ring at most this are allocated. Every ring in S3.1, so every
/// tile; S3.2 lowers it to `ring_max`. The step passes it in its uniforms
/// (`alloc_ring`).
pub(crate) const ALLOC_RING: u32 = u32::MAX;

/// Tiles the pool holds for `cells` (`P`): every tile of the box in S3.1.
pub(crate) fn pool_tiles(cells: [u32; 3]) -> u32 {
    tile_total(cells) as u32
}

/// Words of the tile counts: word 0 the cell set C's size, word k in
/// 1..=ring_max + 1 the tiles with ring ≤ k, then the retired count, the
/// ring halves' parity, and the allocator's four: the free stack's top, this
/// step's pop count, the exhausted flag and the fresh list's length.
pub(super) fn tile_count_words(ring_max: u32) -> u64 {
    u64::from(ring_max) + 8
}

/// Word of the counts holding the free stack's top (the other allocator
/// words follow it).
pub(super) fn free_top_word(ring_max: u32) -> usize {
    ring_max as usize + 4
}

/// Indirect triples for C, each ring cap, retired tiles and full faces,
/// followed by one aligned replay range for the extension layers and the
/// fresh list's triple.
pub(super) fn tile_args_words(ring_max: u32) -> u64 {
    2 * (u64::from(extension_range_index(ring_max)) + 1) + 3
}

/// Bytes of the tile table at `cells` for `ring_max`: the nearness, two
/// ring halves, the list by ring, the retired list, the slot table
/// (`T + P` words), the free stack (`P`), the fresh list (`T`), the counts
/// and the triples.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(crate) fn tile_scratch_bytes(cells: [u32; 3], ring_max: u32) -> u64 {
    let tiles = tile_total(cells);
    let pool = u64::from(pool_tiles(cells));
    (5 * tiles
        + (tiles + pool)
        + pool
        + tiles
        + tile_count_words(ring_max)
        + tile_args_words(ring_max))
        * 4
}

/// Test-only lever, approved 2026-10-02 lead; un-suppressed when the executor
/// exposes node access. With it set, `tiles_classify` marks every tile
/// occupied and `tiles_rings` writes ring 0 everywhere, so the sparse passes
/// run dense through the same kernels and lists (design D-6).
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub(super) static ALL_TILES: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn set_all_tiles(on: bool) {
    ALL_TILES.store(on, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn all_tiles() -> bool {
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    {
        ALL_TILES.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(all(any(test, feature = "testkit"), feature = "gpu-proofs")))]
    {
        false
    }
}

/// The tile table (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3 (The tile
/// table)), built on the GPU every step from the sort's ranges and never
/// read back. Sized for one `ring_max`.
pub(super) struct TileTable {
    pub(super) ring_max: u32,
    /// Per tile, the Chebyshev cell distance from its box to the nearest
    /// particle-holding cell: 0..=CELL_REACH, or CELL_REACH + 1 beyond.
    pub(super) near: GpuBuffer,
    /// Two halves of one tile each, the tile's list rank (0 in C); the
    /// parity word in `counts` names the current half. Zeroed once: the
    /// first step's "previous" ranks are all 0, so every tile outside C is
    /// retired then.
    pub(super) rank: GpuBuffer,
    /// Every tile: C first, then the rest of rings 0 and 1, then ring by ring.
    pub(super) by_ring: GpuBuffer,
    /// [`tile_count_words`]; zero but the free stack's top (every slot free).
    pub(super) counts: GpuBuffer,
    /// [`tile_args_words`] indirect triples.
    pub(super) args: GpuBuffer,
    /// Tiles in C on the previous step and not on this one.
    pub(super) retired: GpuBuffer,
    /// Pool slots the fine-lattice fields hold (`P`).
    pub(super) pool_tiles: u32,
    /// `slot_of` per box tile, then `tile_of` per slot (`T + P` words):
    /// every tile absent in a new table, filled by the allocator.
    pub(super) table: GpuBuffer,
    /// The free slots, `P` words, `free_top` of them live (a counts word);
    /// every slot in a new table.
    pub(super) free_list: GpuBuffer,
    /// The tiles allocated this step, `T` words.
    pub(super) fresh: GpuBuffer,
}

/// Byte offset of the retired list's indirect triple in `args`.
pub(super) fn retired_args_offset(ring_max: u32) -> u64 {
    12 * (u64::from(ring_max) + 1)
}

pub(super) fn face_args_offset(ring_max: u32) -> u64 {
    retired_args_offset(ring_max) + 12
}

/// Byte offset of the fresh list's indirect triple in `args`, after the
/// replay range.
pub(super) fn fresh_args_offset(ring_max: u32) -> u64 {
    8 * (u64::from(extension_range_index(ring_max)) + 1)
}

/// Index of the 8-byte replay range after the indirect triples.
pub(super) fn extension_range_index(ring_max: u32) -> u32 {
    (3 * (ring_max + 3)).div_ceil(2)
}

/// `bound` plus the table and the arrays the retire and the poison write:
/// the gathered faces and the C passes' cell arrays.
pub(super) fn canonical<'a>(
    l: &'a LatticeBuffers,
    table: &'a TileTable,
    mut bound: Vec<GpuBinding<'a>>,
) -> Vec<GpuBinding<'a>> {
    bound.extend([
        buffer(4, &l.g),
        buffer(33, &l.water),
        buffer(34, &l.phi),
        buffer(35, &l.rhs),
        buffer(51, &table.table),
    ]);
    bound
}

/// `bound` plus every pooled field the pool build and a fresh tile set to
/// their canonical values (`pool_canon`, `tiles_fresh`).
pub(super) fn pooled<'a>(
    l: &'a LatticeBuffers,
    mut bound: Vec<GpuBinding<'a>>,
) -> Vec<GpuBinding<'a>> {
    bound.extend([
        buffer(4, &l.g),
        buffer(33, &l.water),
        buffer(34, &l.phi),
        buffer(35, &l.rhs),
        buffer(55, &l.a),
        buffer(56, &l.b),
        buffer(57, &l.f),
        buffer(58, &l.s),
        buffer(59, &l.v),
        buffer(60, &l.faces),
        buffer(61, &l.pressure),
        buffer(62, &l.solve_water),
        buffer(23, &l.pocket),
        buffer(25, &l.pocket_label),
        buffer(41, &l.pocket_leader),
    ]);
    bound
}

/// A shared buffer holding `words`, written before any GPU work sees it.
fn allocate_words(device: &GpuDevice, words: &[u32]) -> Result<GpuBuffer, String> {
    let buffer = allocate_zeroed(device, (words.len() * 4) as u64)?;
    // SAFETY: shared buffer sized for `words`; no GPU work has touched it yet.
    unsafe { buffer.write(0, bytemuck::cast_slice(words)) };
    Ok(buffer)
}

impl TileTable {
    pub(super) fn new(device: &GpuDevice, cells: [u32; 3], ring_max: u32) -> Result<Self, String> {
        let tiles = tile_total(cells) * 4;
        let pool = pool_tiles(cells);
        // Every tile absent, every slot free: the first active step's
        // allocation takes the whole pool.
        let table = vec![TS_NONE; tile_total(cells) as usize + pool as usize];
        let free_list: Vec<u32> = (0..pool).rev().collect();
        let mut counts = vec![0u32; tile_count_words(ring_max) as usize];
        counts[free_top_word(ring_max)] = pool;
        Ok(Self {
            ring_max,
            near: allocate(device, tiles)?,
            rank: allocate_zeroed(device, 2 * tiles)?,
            by_ring: allocate(device, tiles)?,
            counts: allocate_words(device, &counts)?,
            args: allocate(device, tile_args_words(ring_max) * 4)?,
            retired: allocate(device, tiles)?,
            pool_tiles: pool,
            table: allocate_words(device, &table)?,
            free_list: allocate_words(device, &free_list)?,
            fresh: allocate(device, tiles)?,
        })
    }
}

/// The table for this step, after the sort: nearness per tile, rings, the
/// pool's allocation (tiles past `alloc_ring` freed, absent ones within it
/// allocated and set to their canonical values), then the lists, counts,
/// triples and the active-fraction stats word (`capped` at the solver
/// words). An inactive slot of `plan` keeps the last active step's table
/// and zeroes its triples.
pub(super) fn encode_tiles(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    l: &LatticeBuffers,
    t: &TileTable,
    ranges: &GpuBuffer,
    capped: &GpuBuffer,
    tally: u64,
    plan: &GpuBuffer,
) {
    let tiles = groups(tile_total(params.n));
    enc.dispatch_compute(
        &pipes.tiles_classify,
        &[
            uniform(params),
            buffer(1, ranges),
            buffer(27, &t.near),
            buffer(30, &t.counts),
            buffer(46, plan),
        ],
        groups(tile_total(params.n) * 32),
        "gpu_flip.step.tiles.classify",
    );
    enc.dispatch_compute(
        &pipes.tiles_rings,
        &[
            uniform(params),
            buffer(27, &t.near),
            buffer(28, &t.rank),
            buffer(30, &t.counts),
            buffer(46, plan),
        ],
        tiles,
        "gpu_flip.step.tiles.rings",
    );
    enc.dispatch_compute(
        &pipes.tiles_free,
        &[
            uniform(params),
            buffer(28, &t.rank),
            buffer(30, &t.counts),
            buffer(46, plan),
            buffer(52, &t.free_list),
            buffer(54, &t.table),
        ],
        tiles,
        "gpu_flip.step.tiles.free",
    );
    enc.dispatch_compute(
        &pipes.tiles_alloc,
        &[
            uniform(params),
            buffer(28, &t.rank),
            buffer(30, &t.counts),
            buffer(46, plan),
            buffer(52, &t.free_list),
            buffer(53, &t.fresh),
            buffer(54, &t.table),
        ],
        tiles,
        "gpu_flip.step.tiles.alloc",
    );
    enc.dispatch_compute(
        &pipes.tiles_settle,
        &[
            uniform(params),
            buffer(30, &t.counts),
            buffer(31, &t.args),
            buffer(46, plan),
        ],
        [1, 1, 1],
        "gpu_flip.step.tiles.settle",
    );
    enc.dispatch_compute_indirect(
        &pipes.tiles_fresh,
        &pooled(
            l,
            vec![uniform(params), buffer(51, &t.table), buffer(53, &t.fresh)],
        ),
        &t.args,
        fresh_args_offset(t.ring_max),
        "gpu_flip.step.tiles.fresh",
    );
    enc.dispatch_compute(
        &pipes.tiles_lists,
        &[
            uniform(params),
            buffer(46, plan),
            buffer(27, &t.near),
            buffer(28, &t.rank),
            buffer(29, &t.by_ring),
            buffer(30, &t.counts),
            buffer(31, &t.args),
            buffer(32, &t.retired),
            GpuBinding::Buffer {
                binding: 22,
                buffer: capped,
                offset: tally,
            },
        ],
        [1, 1, 1],
        "gpu_flip.step.tiles.lists",
    );
}

/// The canonical block of every pooled field of `l`, once after the pool is
/// built (`pool_canon`).
pub(super) fn encode_pool_canon(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    l: &LatticeBuffers,
) {
    enc.dispatch_compute(
        &pipes.pool_canon,
        &pooled(l, vec![uniform(params)]),
        [1, 1, 1],
        "gpu_flip.step.pool_canon",
    );
}
