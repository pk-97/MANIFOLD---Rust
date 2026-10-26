// The tile table (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3 (The tile
// table)): 8³ tiles, T = ceil(n / 8) per axis, x fastest, partial edge tiles.
// Per tile, the Chebyshev cell distance from its box to the nearest
// particle-holding cell, 0..=CELL_REACH, or CELL_REACH + 1 beyond.
@group(0) @binding(27) var<storage, read_write> tile_near: array<u32>;

// Two halves of one tile each: the tile's list rank (rank_of), 0 in the
// cell set C, 1 for the rest of rings 0 and 1, else its ring (ring_max + 1
// beyond). The parity word of `tile_counts` names the current half.
@group(0) @binding(28) var<storage, read_write> tile_rank: array<u32>;

// Every tile: the cell set C (ring <= 1 and near <= CELL_REACH) first, then
// the rest of rings 0 and 1, then ring by ring (tiles_lists).
@group(0) @binding(29) var<storage, read_write> tiles_by_ring: array<u32>;

// Word 0 |C|, word k = 1..=ring_max + 1 the tiles with ring <= k (the last
// is every tile), the retired count at ring_max + 2, the parity at
// ring_max + 3, then the allocator's words: the free stack's top, this
// step's pop count, the exhausted flag and the fresh list's length
// (`tile_free_top_word` ...). Atomic: the allocator's threads share them.
@group(0) @binding(30) var<storage, read_write> tile_counts: array<atomic<u32>>;

// Indirect triples [2 · count, 1, 1] (512 threads a tile, 256 a group): one
// per count word k = 0..=ring_max, then the retired list, the full-face
// dispatch, an aligned replay range and the fresh list's triple.
@group(0) @binding(31) var<storage, read_write> tile_args: array<u32>;

// Tiles in C on the previous step and not on this one.
@group(0) @binding(32) var<storage, read_write> tiles_retired: array<u32>;

// The cell arrays the C passes own, for the canonical fill and the retire
// (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 4 (The defined-value rule)); the
// gathered faces bind at `faces_out`.
@group(0) @binding(33) var<storage, read_write> tile_water: array<f32>;

@group(0) @binding(34) var<storage, read_write> tile_phi: array<f32>;

@group(0) @binding(35) var<storage, read_write> tile_rhs: array<f32>;

// The pool's free stack: slots no tile holds, `free_top` of them (counts
// word `tile_free_top_word`). Starts as every slot.
@group(0) @binding(52) var<storage, read_write> tile_free: array<u32>;

// The tiles allocated this step, in no order (`tiles_alloc`); their number
// is counts word `tile_fresh_word`.
@group(0) @binding(53) var<storage, read_write> tiles_fresh_list: array<u32>;

// The allocator's own view of `ts_table` (the helpers read it at 51): the
// kernels that write it never read it through the helpers.
@group(0) @binding(54) var<storage, read_write> tile_table: array<u32>;

// The pooled fields `tiles_fresh` sets to their canonical values besides
// the ones above (water, φ, rhs at 33-35, the gathered faces at 4, the
// pockets at 23, 25 and 41): the other face grids, the pressure and the
// solves' water.
@group(0) @binding(55) var<storage, read_write> fresh_a: array<FaceSample>;

@group(0) @binding(56) var<storage, read_write> fresh_b: array<FaceSample>;

@group(0) @binding(57) var<storage, read_write> fresh_f: array<FaceSample>;

@group(0) @binding(58) var<storage, read_write> fresh_s: array<FaceSample>;

@group(0) @binding(59) var<storage, read_write> fresh_v: array<FaceSample>;

@group(0) @binding(60) var<storage, read_write> fresh_faces: array<FaceSample>;

@group(0) @binding(61) var<storage, read_write> fresh_pressure: array<f32>;

@group(0) @binding(62) var<storage, read_write> fresh_solve_water: array<f32>;
