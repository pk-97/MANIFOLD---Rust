//! The step on world tiles: the pool's size, the table's reach, and FLIP's
//! own passes over the table the shared build leaves
//! (`manifold_water_liquid::world_tiles`).

use super::*;
use manifold_node_engine::scene::world_tiles::tile_of_cell;
use manifold_water_liquid::tile_pool::{POOL_BUDGET_BYTES, pool_capacity};
use manifold_water_liquid::tile_storage::{TILE, TILE_CELLS};
use manifold_water_liquid::world_tiles::{
    Allocation, PointCount, Reach, SharedTable, TileClip, allocated_args_offset, fresh_args_offset,
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

/// Bytes per fine cell of every pooled field: the step's own (the lattice's
/// cell arrays and face grids), the solver's vectors and rows, the
/// sheeting's arrays, the sheet distance's two fields and Narrow Band's
/// fields. The bodies hold none (their partials are per slot).
pub(crate) fn pooled_bytes_per_cell() -> u64 {
    STEP_POOLED_BYTES_PER_CELL
        + crate::primitives::gpu_flip_pressure::POOLED_BYTES_PER_CELL
        + crate::primitives::gpu_flip_sheeting::POOLED_BYTES_PER_CELL
        + crate::primitives::gpu_flip_sheet_distance::POOLED_BYTES_PER_CELL
        + super::narrow_band::pooled_bytes_per_cell()
}

/// Cells past the Bounds box water reaches along an axis, both sides
/// together: the walls' 1.5-cell solid band on each side. The pool and the
/// force lattices are sized over the box and this band.
pub const BOX_BAND_CELLS: u32 = 3;

/// Cells past a Bounds box of `n` cells that [`band_box`] can span along an
/// axis: the box as placed straddles at most one cell more than its `n`,
/// and the band adds whole cells on both sides.
pub(crate) const BAND_BOX_EXTRA_CELLS: u32 = 1 + 2 * BOX_BAND_CELLS.div_ceil(2);

manifold_core::testkit_visible! {
/// Tiles the pool holds for a Bounds box of `cells` (`P`) with Wall faces
/// `walls` (bits −X +X −Y +Y −Z +Z): every tile the box and its walls' band
/// can touch, wherever it sits on the world lattice, and the table's rings
/// past each side without a Wall (the [`wall_clip`] holds none past a
/// Wall), so water anywhere in the box never runs the pool short; or what
/// the budget buys.
pub(crate) fn pool_tiles(cells: [u32; 3], walls: u32) -> u32 {
    let ring = u64::from(step_reach().ring_max);
    let wanted = (0..3)
        .map(|a| {
            let open_sides = 2 - ((walls >> (2 * a)) & 0b11).count_ones();
            run_tiles(cells[a] + BAND_BOX_EXTRA_CELLS) + u64::from(open_sides) * ring
        })
        .product();
    pool_capacity(
        wanted,
        u64::from(TILE_CELLS) * pooled_bytes_per_cell(),
        POOL_BUDGET_BYTES,
    )
}
}

/// The most tiles the step's pool may grow to, from the domain's pool
/// ([`pool_tiles`], every tile the Bounds box can touch) and the Wall and
/// Open faces: the domain's pool while a face bounds the water, and with
/// neither (Bounds off) what the pool budget buys, since nothing bounds
/// where the water goes.
pub(crate) fn growth_ceiling(domain_pool: u32, walls: u32, open: u32) -> u32 {
    if walls | open != 0 {
        return domain_pool;
    }
    let budget = pool_capacity(
        u64::MAX,
        u64::from(TILE_CELLS) * pooled_bytes_per_cell(),
        POOL_BUDGET_BYTES,
    );
    domain_pool.max(budget)
}

/// Bytes of the world tiles of a pool of `pool_tiles`: the table, its
/// lists, counts, triples and build scratch.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(crate) fn tile_scratch_bytes(pool_tiles: u32) -> u64 {
    manifold_water_liquid::world_tiles::held_bytes(pool_tiles, step_reach().ring_max)
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
pub(super) fn wall_clip(params: &StepParams) -> Option<TileClip> {
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
        buffer(45, &l.static_corners),
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
/// `particles` (the slots in use `record` names) and Narrow Band's `kept`
/// words, or under
/// [`set_all_tiles`] from one point per box tile, sharing its key set with
/// the World's Matter table (`shared`); then the fresh slots set to
/// their canonical values and the slots that left C retired. The sort keys
/// on the table this leaves. An inactive slot of `plan` keeps the last
/// active step's table and runs none of it.
#[allow(clippy::too_many_arguments)]
pub(super) fn encode_tiles(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    l: &LatticeBuffers,
    tiles: &WorldTiles,
    points: (&GpuBuffer, &GpuBuffer),
    kept: Option<&GpuBuffer>,
    shared: Option<SharedTable<'_>>,
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
            replay_counts: replay_layers(params),
            gate: plan,
            clip: wall_clip(params),
            kept,
            shared,
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

/// The static colliders' corners, after the tile build and `pose_bodies`:
/// the fresh slots', then every held slot's when `static_all` is set or a
/// static body moves (`static_corners_args` takes both), before
/// `solid_corners` reads them.
pub(super) fn encode_static_corners(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    l: &LatticeBuffers,
    tiles: &WorldTiles,
    step: &Step<'_>,
    posed: &GpuBuffer,
) {
    let ring = tiles.reach().ring_max;
    enc.dispatch_compute_indirect(
        &pipes.static_fresh,
        &[
            uniform(params),
            buffer(15, step.shapes),
            buffer(27, step.collider_nodes),
            buffer(28, step.collider_triangles),
            buffer(45, &l.static_corners),
            buffer(46, step.clock_plan),
            buffer(48, posed),
            buffer(51, tiles.table()),
            buffer(53, &tiles.lists().fresh),
        ],
        tiles.args(),
        fresh_args_offset(ring),
        "gpu_flip.step.static_corners.fresh",
    );
    enc.compute_memory_barrier_buffers();
    enc.dispatch_compute(
        &pipes.static_args,
        &[
            uniform(params),
            buffer(31, tiles.args()),
            buffer(46, step.clock_plan),
            buffer(52, &l.static_scratch),
        ],
        [1, 1, 1],
        "gpu_flip.step.static_corners.args",
    );
    enc.compute_memory_barrier_buffers();
    enc.dispatch_compute_indirect(
        &pipes.static_held,
        &[
            uniform(params),
            buffer(15, step.shapes),
            buffer(27, step.collider_nodes),
            buffer(28, step.collider_triangles),
            buffer(29, &tiles.lists().by_rank),
            buffer(45, &l.static_corners),
            buffer(46, step.clock_plan),
            buffer(48, posed),
            buffer(51, tiles.table()),
        ],
        &l.static_scratch,
        STATIC_TRIPLE_OFFSET,
        "gpu_flip.step.static_corners.held",
    );
    enc.compute_memory_barrier_buffers();
}

/// The solid corners of every held slot, from this step's posed bodies, the
/// static corners and the walls (`solid_corners`), before any pass reads the
/// solids.
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
            buffer(45, &l.static_corners),
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
