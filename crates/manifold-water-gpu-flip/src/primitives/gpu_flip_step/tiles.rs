//! The step's tile table (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3 (The tile table)): its sizes, storage and per-step build.

use super::*;
use super::lattice::allocate_zeroed;

manifold_core::testkit_visible! {
/// Cells per tile side (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3 (The tile
/// table)). Partial edge tiles are allowed: no lattice side rule.
pub(crate) const TILE: u32 = 8;
}

/// The cell passes' reach from a particle-holding cell, in cells: the tile
/// set C is every tile within it (`tiles_classify` writes the distance).
/// The shader holds its own copy; the proofs' CPU model reads this one.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub const CELL_REACH: u32 = 2;

manifold_core::testkit_visible! {
/// Tiles per axis, the last one partial when the side is not a multiple of
/// [`TILE`].
pub(crate) fn tile_counts(cells: [u32; 3]) -> [u32; 3] {
    cells.map(|n| n.div_ceil(TILE))
}
}

manifold_core::testkit_visible! {
pub(crate) fn tile_total(cells: [u32; 3]) -> u64 {
    tile_counts(cells).iter().map(|&t| u64::from(t)).product()
}
}

manifold_core::testkit_visible! {
/// The farthest tile ring any extend layer reaches at `band` layers: a
/// layer fills faces `1 + layer` cells from the water. A per-step constant,
/// not a cap: the table is sized for it.
pub(crate) fn ring_max(band: u32) -> u32 {
    (1 + band).div_ceil(TILE)
}
}

/// Words of the tile counts: word 0 the cell set C's size, word k in
/// 1..=ring_max + 1 the tiles with ring ≤ k, then the retired count, then
/// the ring halves' parity.
pub(super) fn tile_count_words(ring_max: u32) -> u64 {
    u64::from(ring_max) + 4
}

/// Indirect triples for C, each ring cap, retired tiles and full faces,
/// followed by one aligned replay range for the extension layers.
pub(super) fn tile_args_words(ring_max: u32) -> u64 {
    2 * (u64::from(extension_range_index(ring_max)) + 1)
}

/// Bytes of the tile table at `cells` for `ring_max`: the nearness, two
/// ring halves, the list by ring, the retired list, the counts and the
/// triples.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(crate) fn tile_scratch_bytes(cells: [u32; 3], ring_max: u32) -> u64 {
    (5 * tile_total(cells) + tile_count_words(ring_max) + tile_args_words(ring_max)) * 4
}

/// Test-only lever, approved 2026-10-02 lead; un-suppressed when the executor
/// exposes node access. With it set, `tiles_classify` marks every tile
/// occupied and `tiles_rings` writes ring 0 everywhere, so the sparse passes
/// run dense through the same kernels and lists (design D-6).
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub(super) static ALL_TILES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn set_all_tiles(on: bool) {
    ALL_TILES.store(on, std::sync::atomic::Ordering::SeqCst);
}

pub(in crate::primitives) fn all_tiles() -> bool {
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
    /// [`tile_count_words`]; zeroed once, for the parity word.
    pub(super) counts: GpuBuffer,
    /// [`tile_args_words`] indirect triples.
    pub(super) args: GpuBuffer,
    /// Tiles in C on the previous step and not on this one.
    pub(super) retired: GpuBuffer,
}

/// Byte offset of the retired list's indirect triple in `args`.
pub(super) fn retired_args_offset(ring_max: u32) -> u64 {
    12 * (u64::from(ring_max) + 1)
}

pub(super) fn face_args_offset(ring_max: u32) -> u64 {
    retired_args_offset(ring_max) + 12
}

/// Index of the 8-byte replay range after the indirect triples.
pub(super) fn extension_range_index(ring_max: u32) -> u32 {
    (3 * (ring_max + 3)).div_ceil(2)
}

/// `bound` plus the arrays the fill, the retire and the poison write: the
/// gathered faces and the C passes' cell arrays.
pub(super) fn canonical<'a>(l: &'a LatticeBuffers, mut bound: Vec<GpuBinding<'a>>) -> Vec<GpuBinding<'a>> {
    bound.extend([buffer(4, &l.g), buffer(33, &l.water), buffer(34, &l.phi), buffer(35, &l.rhs)]);
    bound
}

impl TileTable {
    pub(super) fn new(device: &GpuDevice, cells: [u32; 3], ring_max: u32) -> Result<Self, String> {
        let tiles = tile_total(cells) * 4;
        Ok(Self {
            ring_max,
            near: allocate(device, tiles)?,
            rank: allocate_zeroed(device, 2 * tiles)?,
            by_ring: allocate(device, tiles)?,
            counts: allocate_zeroed(device, tile_count_words(ring_max) * 4)?,
            args: allocate(device, tile_args_words(ring_max) * 4)?,
            retired: allocate(device, tiles)?,
        })
    }
}

/// The table for this step, after the sort: nearness per tile, rings, then
/// the lists, counts, triples and the active-fraction stats word (`capped`
/// at the solver words). An inactive slot of `plan` keeps the last active
/// step's table and zeroes its triples.
pub(super) fn encode_tiles(enc: &mut GpuEncoder, pipes: &Pipelines, params: &StepParams, t: &TileTable, ranges: &GpuBuffer, capped: &GpuBuffer, tally: u64, plan: &GpuBuffer) {
    let tiles = groups(tile_total(params.n));
    enc.dispatch_compute(
        &pipes.tiles_classify,
        &[uniform(params), buffer(1, ranges), buffer(27, &t.near), buffer(30, &t.counts), buffer(46, plan)],
        groups(tile_total(params.n) * 32),
        "gpu_flip.step.tiles.classify",
    );
    enc.dispatch_compute(
        &pipes.tiles_rings,
        &[uniform(params), buffer(27, &t.near), buffer(28, &t.rank), buffer(30, &t.counts), buffer(46, plan)],
        tiles,
        "gpu_flip.step.tiles.rings",
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
