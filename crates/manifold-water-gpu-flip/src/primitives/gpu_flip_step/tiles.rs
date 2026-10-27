//! The step on world tiles: the pool's size, the table's reach, and FLIP's
//! own passes over the table the shared build leaves
//! (`manifold_water_liquid::world_tiles`).

use super::*;
use manifold_node_engine::scene::world_tiles::tile_of_cell;
use manifold_water_liquid::tile_pool::{fitted_pool, grown_pool};
use manifold_water_liquid::tile_storage::{TILE, TILE_CELLS};
use manifold_water_liquid::world_tiles::{
    Allocation, PointCount, Reach, TileClip, allocated_args_offset, fresh_args_offset, held_bytes,
    retired_args_offset, run_tiles,
};

/// The cell passes' reach from a particle-holding cell, in cells: the cell
/// set C is every tile within it. The proofs' CPU model reads it too.
pub const CELL_REACH: u32 = 2;

manifold_core::testkit_visible! {
/// The farthest tile ring any extend layer reaches at `band` layers: a
/// layer fills faces `1 + layer` cells from the water. A per-step constant,
/// not a cap: the table is sized for it.
pub(crate) fn ring_max(band: u32) -> u32 {
    (1 + band).div_ceil(TILE)
}
}

/// The table's reach at the step's band: tiles within `ring_max` of a
/// particle's tile are held, C within [`CELL_REACH`] cells.
pub(super) fn step_reach() -> Reach {
    let ring = ring_max(step_band());
    Reach {
        ring_max: ring,
        alloc_ring: ring,
        cell_reach: CELL_REACH,
    }
}

/// Bytes the pool budget covers for the fine lattice: the D4 live budget
/// (docs/GPU_FLIP_END_GAME_DESIGN.md), 1.5 GiB.
pub(crate) const POOL_BUDGET_BYTES: u64 = 3 << 29;

/// Bytes per fine cell of every pooled field: the step's own (the lattice's
/// cell arrays and face grids), the solver's vectors and rows, the
/// sheeting's arrays and the sheet distance's two fields. The bodies hold
/// none (their partials are per slot).
pub(crate) fn pooled_bytes_per_cell() -> u64 {
    STEP_POOLED_BYTES_PER_CELL
        + crate::primitives::gpu_flip_pressure::POOLED_BYTES_PER_CELL
        + crate::primitives::gpu_flip_sheeting::POOLED_BYTES_PER_CELL
        + crate::primitives::gpu_flip_sheet_distance::POOLED_BYTES_PER_CELL
}

/// Tiles a pool of `budget` bytes holds when a tile costs `tile_bytes`, at
/// most the `wanted` tiles.
pub(crate) fn pool_capacity(wanted: u64, tile_bytes: u64, budget: u64) -> u32 {
    wanted.min(budget / tile_bytes) as u32
}

/// Cells past the Bounds box water reaches along an axis, both sides
/// together: the walls' 1.5-cell solid band on each side. The pool and the
/// force lattices are sized over the box and this band.
pub const BOX_BAND_CELLS: u32 = 3;

manifold_core::testkit_visible! {
/// Tiles the pool holds for a Bounds box of `cells` (`P`): every tile the
/// box and its walls' band can touch, wherever it sits on the world
/// lattice, and the table's rings round them, so water anywhere in the box
/// never runs the pool short; or what the budget buys.
pub(crate) fn pool_tiles(cells: [u32; 3]) -> u32 {
    let ring = u64::from(step_reach().ring_max);
    let wanted = cells
        .iter()
        .map(|&n| run_tiles(n + BOX_BAND_CELLS) + 2 * ring)
        .product();
    pool_capacity(
        wanted,
        u64::from(TILE_CELLS) * pooled_bytes_per_cell(),
        POOL_BUDGET_BYTES,
    )
}
}

/// The pool a tick runs on and whether it is fitted yet, from `ceiling`
/// ([`pool_tiles`], every tile the box can touch), the last tick's pool and
/// fit (`None` before the first tick) and its table's late read (held
/// tiles, short). The first tick runs on the ceiling, so any fill fits; the
/// first read that holds water fits the pool to it, and from then on it
/// only grows, so a restart's fill finds the room the first one had. Never
/// past the ceiling. The passes launch over the whole pool, so a pool that
/// follows the water costs what the water does.
pub(super) fn next_pool(
    ceiling: u32,
    last: Option<(u32, bool)>,
    read: Option<(u32, bool)>,
) -> (u32, bool) {
    let (pool, fitted) = match (last, read) {
        (None, _) => (ceiling, false),
        (Some((pool, true)), Some((held, short))) => {
            (grown_pool(pool, held, short).unwrap_or(pool), true)
        }
        (Some((_, false)), Some((held, _))) if held > 0 => (fitted_pool(held), true),
        (Some(last), _) => last,
    };
    (pool.min(ceiling), fitted)
}

/// Bytes of the world tiles of a pool of `pool_tiles`: the table, its
/// lists, counts, triples and build scratch.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(crate) fn tile_scratch_bytes(pool_tiles: u32) -> u64 {
    held_bytes(pool_tiles, step_reach().ring_max)
}

/// Test-only lever. With it set, the table is built from one point in every
/// tile the Bounds box and its walls' band touch instead of the particles,
/// so every such tile is in C and
/// the passes run dense through the same kernels and lists: the proofs'
/// oracle for the sparse lists.
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

/// One liquid record at the centre of a cell in every world tile the box of
/// `cells` from world cell `lo` touches, and their record
/// `[groups, 1, 1, count]`: the [`set_all_tiles`] table's points.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub(super) fn box_points(
    device: &GpuDevice,
    lo: [i32; 3],
    cells: [u32; 3],
    cell_size: f32,
) -> Result<(GpuBuffer, GpuBuffer), String> {
    use manifold_node_engine::scene::world_tiles::tile_of_cell;
    let last: [i32; 3] = std::array::from_fn(|a| lo[a] + cells[a] as i32 - 1);
    let (first, end) = (tile_of_cell(lo), tile_of_cell(last));
    let mut points = Vec::new();
    for z in first[2]..=end[2] {
        for y in first[1]..=end[1] {
            for x in first[0]..=end[0] {
                // The tile's first cell, clamped into the box.
                let cell: [i32; 3] =
                    std::array::from_fn(|a| ([x, y, z][a] * TILE as i32).clamp(lo[a], last[a]));
                points.push(FluidParticle {
                    position_radius: [
                        cell[0] as f32 * cell_size,
                        cell[1] as f32 * cell_size,
                        cell[2] as f32 * cell_size,
                        1.0,
                    ],
                    ..FluidParticle::default()
                });
            }
        }
    }
    let count = points.len() as u32;
    let records = allocate_zeroed(device, u64::from(count) * size_of::<FluidParticle>() as u64)?;
    let record = allocate_zeroed(device, SLOT_RECORD_BYTES)?;
    // SAFETY: two new shared buffers sized for what is written; no GPU work
    // has touched them.
    unsafe {
        records.write(0, bytemuck::cast_slice(&points));
        record.write(
            0,
            bytemuck::cast_slice(&[count.div_ceil(256).max(1), 1, 1, count]),
        );
    }
    Ok((records, record))
}

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
impl StepState {
    /// Under [`set_all_tiles`], the points of the Bounds box `params` holds,
    /// widened by its walls' band; none otherwise.
    pub(super) fn reserve_box_points(
        &mut self,
        device: &GpuDevice,
        params: &StepParams,
    ) -> Result<(), String> {
        self.box_points = if all_tiles() {
            let (lo, cells) = band_box(params);
            Some(box_points(device, lo, cells, params.cell_size)?)
        } else {
            None
        };
        Ok(())
    }
}

/// The tiles the step's table may hold past the water's own: on an axis side
/// with a Wall face, none past the tile holding the wall's band, so no ring
/// tile lies inside a wall or under the floor. An Open face, or no Bounds,
/// clips nothing on its side.
fn wall_clip(params: &StepParams) -> Option<TileClip> {
    if params.wall_faces == 0 {
        return None;
    }
    let (lo, cells) = band_box(params);
    let first = tile_of_cell(lo);
    let last = tile_of_cell(std::array::from_fn(|a| lo[a] + cells[a] as i32 - 1));
    let mut clip = [[i32::MIN; 3], [i32::MAX; 3]];
    for a in 0..3 {
        if params.wall_faces & (1 << (2 * a)) != 0 {
            clip[0][a] = first[a];
        }
        if params.wall_faces & (1 << (2 * a + 1)) != 0 {
            clip[1][a] = last[a];
        }
    }
    Some(clip)
}

/// The world cells of the Bounds box and its walls' band: the first cell
/// and the count per axis.
fn band_box(params: &StepParams) -> ([i32; 3], [u32; 3]) {
    let cell = |x: f32| x * params.inv_cell;
    let band = BOX_BAND_CELLS.div_ceil(2) as i32;
    let lo: [i32; 3] = std::array::from_fn(|a| cell(params.bounds_lo[a]).floor() as i32 - band);
    let cells = std::array::from_fn(|a| {
        (cell(params.bounds_hi[a]).ceil() as i32 + band - lo[a]).max(1) as u32
    });
    (lo, cells)
}

/// `bound` plus the table and the arrays the retire and the poison write:
/// the gathered faces and the C passes' cell arrays.
pub(super) fn canonical<'a>(
    l: &'a LatticeBuffers,
    tiles: &'a WorldTiles,
    mut bound: Vec<GpuBinding<'a>>,
) -> Vec<GpuBinding<'a>> {
    bound.extend([
        buffer(4, &l.g),
        buffer(33, &l.water),
        buffer(34, &l.phi),
        buffer(35, &l.rhs),
        buffer(51, tiles.table()),
    ]);
    bound
}

/// `bound` plus every pooled field the pool build and a fresh slot set to
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
        buffer(43, &l.corners),
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

/// The step's tiles for this step, before the sort: the shared build from
/// `particles` (the slots in use `record` names), or under
/// [`set_all_tiles`] from one point per box tile; then the fresh slots set to
/// their canonical values and the slots that left C retired. The sort keys
/// on the table this leaves. An inactive slot of `plan` keeps the last
/// active step's table and runs none of it.
pub(super) fn encode_tiles(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    l: &LatticeBuffers,
    tiles: &WorldTiles,
    points: (&GpuBuffer, &GpuBuffer),
    plan: &GpuBuffer,
) {
    let (points, record) = points;
    tiles.encode_allocation(
        enc,
        &Allocation {
            points,
            count: PointCount::Record(record),
            origin: params.origin,
            inv_cell: params.inv_cell,
            replay_count: params.extension_layers,
            gate: plan,
            clip: wall_clip(params),
        },
    );
    let ring = tiles.reach().ring_max;
    enc.dispatch_compute_indirect(
        &pipes.tiles_fresh,
        &pooled(
            l,
            vec![
                uniform(params),
                buffer(51, tiles.table()),
                buffer(53, &tiles.lists().fresh),
            ],
        ),
        tiles.args(),
        fresh_args_offset(ring),
        "gpu_flip.step.tiles.fresh",
    );
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    if POISON.load(std::sync::atomic::Ordering::SeqCst) {
        enc.dispatch_compute(
            &pipes.poison_free,
            &pooled(l, vec![uniform(params), buffer(51, tiles.table())]),
            groups(u64::from(tiles.pool_tiles()) * u64::from(TILE_CELLS)),
            "gpu_flip.step.tiles.poison_free",
        );
    }
    enc.dispatch_compute_indirect(
        &pipes.tiles_retire,
        &canonical(
            l,
            tiles,
            vec![uniform(params), buffer(32, &tiles.lists().retired)],
        ),
        tiles.args(),
        retired_args_offset(ring),
        "gpu_flip.step.tiles.retire",
    );
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    if POISON.load(std::sync::atomic::Ordering::SeqCst) {
        enc.dispatch_compute_indirect(
            &pipes.poison,
            &canonical(
                l,
                tiles,
                vec![
                    uniform(params),
                    buffer(29, &tiles.lists().by_rank),
                    buffer(30, tiles.counts()),
                ],
            ),
            tiles.args(),
            allocated_args_offset(ring),
            "gpu_flip.step.tiles.poison",
        );
    }
}

/// The solid corners of every held slot, from this step's posed bodies and
/// the walls (`solid_corners`), before any pass reads the solids.
pub(super) fn encode_solid_corners(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    l: &LatticeBuffers,
    tiles: &WorldTiles,
    step: &Step<'_>,
    posed: &GpuBuffer,
) {
    enc.dispatch_compute_indirect(
        &pipes.solid_corners,
        &[
            uniform(params),
            buffer(15, step.shapes),
            buffer(16, step.atlas),
            buffer(29, &tiles.lists().by_rank),
            buffer(43, &l.corners),
            buffer(46, step.clock_plan),
            buffer(48, posed),
            buffer(51, tiles.table()),
        ],
        tiles.args(),
        allocated_args_offset(tiles.reach().ring_max),
        "gpu_flip.step.solid_corners",
    );
}

/// After the step's first sort: the pool's held share into solver word 6 and
/// the particles with no slot into the tick's removed count (`tiles_tally`,
/// `capped` at the solver words).
pub(super) fn encode_tile_tally(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    tiles: &WorldTiles,
    ranges: &GpuBuffer,
    step: &Step<'_>,
) {
    enc.dispatch_compute(
        &pipes.tiles_tally,
        &[
            uniform(params),
            buffer(1, ranges),
            buffer(30, tiles.counts()),
            buffer(46, step.clock_plan),
            GpuBinding::Buffer {
                binding: 22,
                buffer: step.capped,
                offset: step.tally,
            },
        ],
        [1, 1, 1],
        "gpu_flip.step.tiles.tally",
    );
}

/// The canonical element of every pooled field of `l`, once after the pool is
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
