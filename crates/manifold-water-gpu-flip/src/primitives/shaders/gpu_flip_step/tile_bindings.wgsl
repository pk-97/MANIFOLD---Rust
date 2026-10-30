// The world tiles' lists, counts and triples the step's passes read
// (manifold_water_liquid::world_tiles, built every active step before the
// sort). Lists hold slots.

// Held slots by rank: the cell set C (rank 0) first, then rank by rank.
@group(0) @binding(29) var<storage, read> tiles_by_ring: array<u32>;

// Word k <= ring_max + 1: the held slots with rank <= k (word 0 is |C|,
// word ring_max every held slot); then the retired count, the free stack's
// top, the exhausted bits and the fresh count (world_tiles.rs
// `retired_word` and on).
@group(0) @binding(30) var<storage, read> tile_counts: array<u32>;

// Indirect triples [2 · count, 1, 1] (512 threads a slot, 256 a group): list
// k at word 3k for k <= ring_max (the one at ring_max covers every held
// slot), the retired list at 3(ring_max + 1), the replay range, the fresh
// list's triple.
@group(0) @binding(31) var<storage, read> tile_args: array<u32>;

// Slots carried from C last build into a rank past 0.
@group(0) @binding(32) var<storage, read> tiles_retired: array<u32>;

// The cell arrays the C passes own, for the canonical fill and the retire
// (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 4 (The defined-value rule)); the
// gathered faces bind at `faces_out`.
@group(0) @binding(33) var<storage, read_write> tile_water: array<f32>;

@group(0) @binding(34) var<storage, read_write> tile_phi: array<f32>;

@group(0) @binding(35) var<storage, read_write> tile_rhs: array<f32>;

// The solid corners, written by `solid_corners` over every held slot; the
// canonical element is `CANON_CORNER`.
@group(0) @binding(43) var<storage, read_write> tile_corners: array<f32>;

// A Matter grain as the water sees it (fluid_particles.rs MatterGrain): its
// bulk's sphere, its velocity and the share of its bulk its grains fill.
struct MatterGrain {
    position_radius: vec4<f32>,
    velocity_solid: vec4<f32>,
}

// A wired Sand's grains this frame, `u.sand_count` of them.
@group(0) @binding(72) var<storage, read> sand_points: array<MatterGrain>;

// The sand at each held cell's low corner, SAND_WORDS words a corner: its
// volume share in SAND_FIXED units, then its momentum along x, y and z in
// SAND_MOMENTUM_FIXED units (i32 in the u32 words), then its grains' solid
// share in SAND_FIXED units. Added by `sand_corners`, read by `sand_drag`
// and taken (reset to 0) by `sand_porous` in the same step.
const SAND_WORDS: u32 = 5u;
@group(0) @binding(73) var<storage, read_write> sand_share: array<atomic<u32>>;

// Slots handed out this build (world_tiles.rs TileLists::fresh).
@group(0) @binding(53) var<storage, read> tiles_fresh_list: array<u32>;

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
