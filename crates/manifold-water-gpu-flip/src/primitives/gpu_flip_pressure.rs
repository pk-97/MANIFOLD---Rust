//! Ported from FLIP Fluids pressuresolver.cpp and pcgsolver.h (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
//! Multigrid preconditioner after McAdams, Sifakis & Teran 2010.
//! GPU FLIP's pressure solver: the multigrid-preconditioned conjugate
//! gradient for L p = f on the water, as a module the step node encodes
//! directly. One solver serves every solve on one pool of world tiles:
//! [`PressureSolver::prepare`] builds the coarse levels and assembles every
//! level's operator rows once (six face weights and the ghost and plain
//! diagonals per cell), then [`PressureSolver::solve`] runs per right-hand
//! side (the pressure solve and the density solve) reading rows only.
//!
//! Every level is a world-tiled lattice. The fine level is the step's table and fields. Level L's table
//! is built at every prepare from the parents (tile >> 1) of level L − 1's
//! held tiles, in a pool of ⌈P_(L−1) / 2⌉ slots ([`level_pools`]), and its
//! fields are pooled in it. The depth is fixed per pool: the levels halving a
//! cube of ⌈P^⅓⌉ tiles a side takes to reach [`COARSEST_SIDE`] cells. The
//! coarsest level is smoothed [`COARSEST_SWEEPS`] red-black sweeps from zero
//! and as many back, a symmetric approximate solve. A coarse level that runs
//! out of slots leaves some tiles of the level below with no parent:
//! restriction and prolongation skip the same tiles, so the V-cycle stays
//! symmetric, and the level's table reports the shortfall in its counts.
//!
//! A solve on the fine level without bodies starts from the pressure its
//! buffer holds (the warm start): the last tick's answer is most of this
//! tick's, so the stop, relative to |f|∞, comes sooner.
//!
//! A solve may run its gradient on a coarse level instead of the fine one
//! ([`Solve::level`], docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 11
//! (Solve Level)): the right-hand side is restricted down to it by the
//! V-cycle's own transfer, the gradient and the V-cycle below it run on that
//! level's rows and slots, and the pressure is prolonged back up. The
//! coarsest level is never a gradient level.

use manifold_gpu::BufferUse;
use manifold_gpu::{
    GATED_RANGE_BYTES, GatedRecorder, GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice,
    GpuEncoder, GpuTemplateStore, TemplateRanges,
};
use manifold_node_engine::scene::world_tiles::{READ_SIDE, TILE, TILE_CELLS, pool_len};
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::tile_storage::TILE_GRID;
use manifold_water_liquid::world_tiles::{WorldTiles, allocated_args_offset};

use super::gpu_flip_bodies::{Bodies, BodyGate, BodyPasses};

const SHADER_BODY: &str = include_str!("shaders/gpu_flip_pressure.wgsl");

/// The solve's kernels with the world tile read side every level is looked
/// up through, and the dense grid helpers of the Lentine gather's boxes.
pub(super) fn shader() -> String {
    format!("{READ_SIDE}\n{TILE_GRID}\n{SHADER_BODY}")
}

/// The coarsest level's side in cells of the cube the depth is taken from
/// ([`level_pools`]).
const COARSEST_SIDE: u32 = 4;
/// Red-black sweeps the coarsest level takes each way in a V-cycle: from
/// zero red then black, then as many black then red, so the level's
/// approximate solve stays symmetric. One workgroup runs them all in one
/// dispatch (gpu_flip_pressure.wgsl coarsest_main).
const COARSEST_SWEEPS: u32 = 8;
manifold_core::testkit_visible! {
/// Iterations one solve may run, FLIP Fluids' cap: the scalars buffer holds
/// two per iteration. Rounds past the stop cost nothing: the template executes
/// only the chunks the GPU-written ranges name.
pub const MAX_ITERATIONS: u32 = 900;
}
/// The stop's relative tolerance on |r|∞ / |f|∞: near where an f32
/// pressure's own residual floors, so further iterations would only lower
/// the recursive residual the stop reads, not the pressure's error
/// (`pressure_module_converges_on_the_engine_tolerance`).
pub(crate) const TOLERANCE: f32 = 1e-5;
/// Red-black rounds before and after the coarse correction on every level
/// but the coarsest.
const SMOOTH_ROUNDS: usize = 2;
const THREADS: u32 = 256;
/// Params mode bit: the pass folds its per-cell product into one partial per
/// workgroup (gpu_flip_pressure.wgsl REDUCE).
const REDUCE: u32 = 2;
/// restrict's mode bits (gpu_flip_pressure.wgsl restrict_main): the fine
/// taps are masked as init masks the right-hand side (water with an open
/// face); the sum is added to `out` instead of written.
const RESTRICT_MASK: u32 = 1;
const RESTRICT_ADD: u32 = 4;
/// init's and check's mode bit (gpu_flip_pressure.wgsl WARM): init keeps the
/// pressure as x0, check is the warm start's test.
const WARM: u32 = 4;

/// The smallest side s with s³ ≥ `tiles`.
pub(crate) fn cube_side(tiles: u32) -> u32 {
    let mut side = 1u32;
    while u64::from(side).pow(3) < u64::from(tiles) {
        side += 1;
    }
    side
}

/// Every level's pool, finest first: the fine pool P, then ⌈P_(L−1) / 2⌉
/// per coarse level. There are as many levels as halving, rounding up, a
/// cube of ⌈P^⅓⌉ tiles a side takes until its side is [`COARSEST_SIDE`]
/// cells or less: at least two.
pub(crate) fn level_pools(pool_tiles: u32) -> Vec<u32> {
    let mut side = cube_side(pool_tiles) * TILE;
    let mut pools = vec![pool_tiles];
    loop {
        side = side.div_ceil(2);
        let last = *pools.last().expect("the fine pool");
        pools.push(last.div_ceil(2));
        if side <= COARSEST_SIDE {
            return pools;
        }
    }
}

/// The deepest level a solve's gradient may run on with a pool of
/// `pool_tiles`: every level but the coarsest, which is only smoothed.
pub(crate) fn max_solve_level(pool_tiles: u32) -> usize {
    level_pools(pool_tiles).len() - 2
}

/// Why `level` is refused with a pool of `pool_tiles`, or None.
pub(crate) fn level_refusal(pool_tiles: u32, level: usize) -> Option<String> {
    let max = max_solve_level(pool_tiles);
    (level > max).then(|| {
        format!("Coarse Solve must be 0 to {max} with a pool of {pool_tiles} tiles, not {level}")
    })
}

/// Each level's first word in the flags and lists (one a slot), then the
/// total.
fn slot_bases(pools: &[u32]) -> Vec<u32> {
    let mut bases = Vec::with_capacity(pools.len() + 1);
    let mut base = 0;
    for &pool in pools {
        bases.push(base);
        base += pool;
    }
    bases.push(base);
    bases
}

/// Device bytes the solver holds for itself with a fine pool of
/// `pool_tiles`: the fine vectors and rows, each coarse level's fields,
/// parents and table, every level's flags and lists, the partial sums and
/// the scalars.
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(crate) fn scratch_bytes(pool_tiles: u32) -> u64 {
    let pools = level_pools(pool_tiles);
    let coarse: u64 = pools
        .windows(2)
        .map(|pair| {
            COARSE_BYTES_PER_CELL * pool_len(pair[1])
                + u64::from(pair[0]) * 4
                + manifold_water_liquid::world_tiles::parents_bytes(pair[1])
        })
        .sum();
    let slots = u64::from(*slot_bases(&pools).last().expect("the total"));
    let slots_of = Slots {
        levels: pools.len(),
    };
    FINE_BYTES_PER_CELL * pool_len(pool_tiles)
        + coarse
        + 2 * slots * 4
        + u64::from(partial_count(pool_tiles)) * 4
        + u64::from(2 * MAX_ITERATIONS) * 4
        + PROGRESS_BYTES
        + u64::from(slots_of.armed() + slots_of.triples()) * TRIPLE_BYTES
        + RANGES_BYTES
}

const FACE_BYTES: u64 = size_of::<FaceSample>() as u64;
/// One cell's operator row (gpu_flip_pressure.wgsl Row): six weights and
/// two diagonals.
const ROW_BYTES: u64 = 32;

/// Fine vectors the solver pools (`r z p scratch body`).
const POOLED_VECTORS: u64 = 5;

/// Bytes per fine cell of the solver's fine fields: the vectors and rows.
const FINE_BYTES_PER_CELL: u64 = POOLED_VECTORS * 4 + ROW_BYTES;

/// Bytes per cell of a coarse level's fields: water, touched, faces, rows,
/// right-hand side and correction.
const COARSE_BYTES_PER_CELL: u64 = 4 * 4 + ROW_BYTES + FACE_BYTES;

/// Bytes per fine cell the solver's pooled fields take: the fine fields and,
/// since the coarse pools sum to about the fine one, the coarse fields once.
pub(crate) const POOLED_BYTES_PER_CELL: u64 = FINE_BYTES_PER_CELL + COARSE_BYTES_PER_CELL;

/// Dispatches one [`PressureSolver::prepare`] and one fine-level
/// [`PressureSolver::solve`] without bodies on `stop` (its cap's rounds)
/// encode with a pool of `pool_tiles`, the world tile builds aside.
#[cfg(test)]
pub(crate) fn passes(pool_tiles: u32, stop: Stop) -> (usize, usize) {
    let coarse = level_pools(pool_tiles).len() - 1;
    let (before, after) = round_commands(coarse, BodyRound::Without);
    // Prepare: every level's flags, list and rows, and each coarse level's
    // parents, water and faces. The solve: arming the gate, init and the
    // start's check, the warm start's residual and, on the engine's stop,
    // its check, then per iteration.
    let prelude = 4 + usize::from(stop.tolerance() >= 0.0);
    (
        6 * coarse + 3,
        prelude + stop.cap() as usize * (before + after) as usize,
    )
}

/// Where a solve's body product sits in its rounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyRound {
    /// No bodies: the operator pass folds p · s and the round is one piece.
    Without,
    /// The body passes share the round's single gated segment.
    Gated,
    /// The body product, gated, on the fine level of a solve at coarse
    /// level k: the direction prolonged down and the product restricted up
    /// inside the round, every pass on the gate.
    GatedCoarse(usize),
}

/// A conjugate gradient round's gated dispatches with `coarse` levels below
/// the gradient's: those up to and including the operator product, then
/// those after it. Gated body passes fit in the first piece. With bodies
/// p · s needs its own pass after the body product; without, the operator
/// pass folds it.
fn round_commands(coarse: usize, bodies: BodyRound) -> (u32, u32) {
    let v_cycle = (coarse * (4 * SMOOTH_ROUNDS + 3) + 1) as u32;
    // v_cycle (r·z folded), finalize, direction, apply | update (|r|∞ folded), check
    match bodies {
        // apply, impulse partial, finalize, body product, p·s partial, finalize
        BodyRound::Gated => (v_cycle + 10, 0),
        // k zeroes and k prolongations down, the fine body zero, k
        // restrictions up
        BodyRound::GatedCoarse(k) => (v_cycle + 11 + 3 * k as u32, 0),
        // apply folds p·s | finalize
        BodyRound::Without => (v_cycle + 6, 0),
    }
}

manifold_core::testkit_visible! {
/// The water a solve runs on, as pooled fields of the fine world tiles
/// every call that takes a `Water` also takes: water per cell (> 0.5), the
/// face records of open fractions (the step's open_fractions pass), the
/// cell size in metres, and the particles' signed distance per cell for the
/// free surface's ghost rows. With no φ the rows are the plain ones, as the
/// density solve runs; the coarse levels always are.
pub(crate) struct Water<'a> {
    pub cell_size: f32,
    pub water: &'a GpuBuffer,
    pub faces: &'a GpuBuffer,
    pub phi: Option<&'a GpuBuffer>,
}
}

impl<'a> Water<'a> {
    /// The φ binding and flag the fine rows are built with. With no φ the
    /// water array sits in the slot and is never read.
    fn ghost(&self) -> (u32, &'a GpuBuffer) {
        self.phi.map_or((0, self.water), |phi| (1, phi))
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    /// The dispatched level's pool P: the level the read side looks cells up
    /// in (its H is the table's own).
    pool_tiles: u32,
    _pad_h: u32,
    /// A transfer's or coarsening's coarse side: its P and H.
    coarse_pool_tiles: u32,
    coarse_entries: u32,
    color: u32,
    mode: u32,
    cell_size: f32,
    slot: u32,
    ghost: u32,
    tolerance: f32,
    /// The listed level's first word in the flags and lists.
    list_base: u32,
    /// The listed level's gate triple.
    level: u32,
    /// classify: every held slot active (the proofs' oracle).
    all_tiles: u32,
    /// Inside a round: the solve-live triple the singleton passes and the
    /// zero check before writing (gpu_flip_pressure.wgsl stopped); 0
    /// outside rounds, where they always run.
    live: u32,
    /// arm and check: the gate's triples; arm: those `armed` holds, and the
    /// bodies.
    triples: u32,
    armed_triples: u32,
    bodies: u32,
    /// restrict: the fine side's first word in the flags.
    below_base: u32,
    /// arm and check: the single executes of the round layout.
    singles: u32,
    _pad: u32,
}

impl Params {
    fn at(cell_size: f32) -> Self {
        Self {
            cell_size,
            ..Self::default()
        }
    }

    /// The level the dispatch looks its cells up in.
    fn table_of(mut self, tiles: &WorldTiles) -> Self {
        self.pool_tiles = tiles.pool_tiles();
        self
    }

    /// A transfer's or coarsening's coarse side.
    fn coarse_side(mut self, tiles: &WorldTiles) -> Self {
        self.coarse_pool_tiles = tiles.pool_tiles();
        self.coarse_entries = tiles.table_entries();
        self
    }

    /// The level whose active list the dispatch runs over.
    fn listed_on(mut self, v: &View<'_>) -> Self {
        self.list_base = v.list_base;
        self.level = v.level;
        self
    }

    /// The level the dispatch runs over and looks its cells up in.
    fn over(self, v: &View<'_>) -> Self {
        self.table_of(v.tiles).listed_on(v)
    }
}

struct Pipelines {
    smooth: GpuComputePipeline,
    coarsest: GpuComputePipeline,
    residual: GpuComputePipeline,
    restrict: GpuComputePipeline,
    prolong: GpuComputePipeline,
    parents: GpuComputePipeline,
    coarsen_water: GpuComputePipeline,
    coarsen_faces: GpuComputePipeline,
    rows: GpuComputePipeline,
    init: GpuComputePipeline,
    warm: GpuComputePipeline,
    dot_partial: GpuComputePipeline,
    dot_finalize: GpuComputePipeline,
    direction: GpuComputePipeline,
    update: GpuComputePipeline,
    check: GpuComputePipeline,
    tally: GpuComputePipeline,
    arm: GpuComputePipeline,
    classify: GpuComputePipeline,
    lists: GpuComputePipeline,
    zero: GpuComputePipeline,
    #[cfg(all(test, feature = "gpu-proofs"))]
    poison: GpuComputePipeline,
}

impl Pipelines {
    fn new(device: &GpuDevice) -> Self {
        let source = shader();
        let pipeline =
            |entry: &str, label: &str| device.create_compute_pipeline(&source, entry, label);
        Self {
            classify: pipeline("classify_main", "gpu_flip.pressure.classify"),
            lists: pipeline("lists_main", "gpu_flip.pressure.lists"),
            #[cfg(all(test, feature = "gpu-proofs"))]
            poison: pipeline("poison_main", "gpu_flip.pressure.poison"),
            smooth: pipeline("smooth_main", "gpu_flip.pressure.smooth"),
            coarsest: pipeline("coarsest_main", "gpu_flip.pressure.coarsest"),
            residual: pipeline("residual_main", "gpu_flip.pressure.residual"),
            restrict: pipeline("restrict_main", "gpu_flip.pressure.restrict"),
            prolong: pipeline("prolong_main", "gpu_flip.pressure.prolong"),
            parents: pipeline("parents_main", "gpu_flip.pressure.parents"),
            coarsen_water: pipeline("coarsen_water_main", "gpu_flip.pressure.coarsen_water"),
            coarsen_faces: pipeline("coarsen_faces_main", "gpu_flip.pressure.coarsen_faces"),
            rows: pipeline("rows_main", "gpu_flip.pressure.rows"),
            init: pipeline("init_main", "gpu_flip.pressure.init"),
            warm: pipeline("warm_main", "gpu_flip.pressure.warm"),
            dot_partial: pipeline("dot_partial_main", "gpu_flip.pressure.dot_partial"),
            dot_finalize: pipeline("dot_finalize_main", "gpu_flip.pressure.dot_finalize"),
            direction: pipeline("direction_main", "gpu_flip.pressure.direction"),
            update: pipeline("update_main", "gpu_flip.pressure.update"),
            check: pipeline("check_main", "gpu_flip.pressure.check"),
            tally: pipeline("tally_main", "gpu_flip.pressure.tally"),
            arm: pipeline("arm_main", "gpu_flip.pressure.arm"),
            zero: pipeline("zero_main", "gpu_flip.pressure.zero"),
        }
    }
}

/// A coarse level: its world tiles and its pooled fields.
struct Level {
    /// Built at every prepare from the level below's held tiles.
    tiles: WorldTiles,
    /// Per slot of the level below, the slot of its tile's parent here
    /// (WT_NONE when free or past the pool), written at prepare.
    parents: GpuBuffer,
    cell_size: f32,
    /// The masks and faces are read at their canonical element (a tile
    /// with no slot), so they start at zero and no pass writes it.
    water: GpuBuffer,
    /// 1 where any child is touched: the level's slot classifier reads it
    /// as its water.
    touched: GpuBuffer,
    faces: GpuBuffer,
    rows: GpuBuffer,
    rhs: GpuBuffer,
    e: GpuBuffer,
}

/// Everything sized by the fine pool, rebuilt when it changes.
struct Buffers {
    /// The fine pool P the fine vectors and rows are sized for.
    pool_tiles: u32,
    coarse: Vec<Level>,
    /// The fine level's operator rows, built at prepare. The fine vectors
    /// below and the rows are pooled fields of the fine pool; a solve on a
    /// coarse level uses r, z, p and the scratch as pooled fields of that
    /// level, whose pool is smaller.
    rows: GpuBuffer,
    r: GpuBuffer,
    z: GpuBuffer,
    p: GpuBuffer,
    /// A V-cycle level's residual, then the iteration's s = −L p.
    scratch: GpuBuffer,
    /// A coarse-level solve with bodies: the fine body product on the
    /// prolonged direction, before it is restricted into s.
    body: GpuBuffer,
    /// Two floats per slot of the gradient's level: a folded reduction's
    /// partials.
    partials: GpuBuffer,
    scalars: GpuBuffer,
    /// The stop's record (gpu_flip_pressure.wgsl check_main).
    progress: GpuBuffer,
    /// Every level's slot flags (1 active) and active-slot lists, level
    /// after level from `bases`, built at prepare.
    flags: GpuBuffer,
    lists: GpuBuffer,
    bases: Vec<u32>,
    /// The level triples and the single one, [`Slots`] order: each level's
    /// written by its list builder at prepare, the single one once.
    armed: GpuBuffer,
    /// What a recorded dispatch runs, by triple: every slot, so a replayed
    /// solve covers any list; the kernels return past the live count.
    groups: Vec<[u32; 3]>,
    /// The solve's live copy of `armed`, zeroed when the solve stops.
    gate: GpuBuffer,
    /// The replayed rounds' range entries (`GATED_RANGE_BYTES` each, two a
    /// round): armed with the gate, and zeroed past the round that stops.
    ranges: GpuBuffer,
}

impl Buffers {
    fn new(device: &GpuDevice, pool_tiles: u32) -> Result<Self, String> {
        let pools = level_pools(pool_tiles);
        let zeroed = |bytes: u64| {
            let buffer = device.create_buffer(bytes, BufferUse::Upload);
            buffer.zero_fill();
            buffer
        };
        let mut coarse = Vec::with_capacity(pools.len() - 1);
        for pair in pools.windows(2) {
            let mut tiles = WorldTiles::new_parents(device, pair[1])?;
            tiles.prepare(device);
            let cells = pool_len(pair[1]);
            coarse.push(Level {
                tiles,
                parents: device.create_buffer(u64::from(pair[0]) * 4, BufferUse::Gpu),
                cell_size: 0.0,
                water: zeroed(cells * 4),
                touched: zeroed(cells * 4),
                faces: zeroed(cells * FACE_BYTES),
                rows: device.create_buffer(cells * ROW_BYTES, BufferUse::Gpu),
                rhs: device.create_buffer(cells * 4, BufferUse::Gpu),
                e: device.create_buffer(cells * 4, BufferUse::Gpu),
            });
        }
        let bases = slot_bases(&pools);
        let slots = u64::from(*bases.last().expect("the total"));
        let vector = || device.create_buffer(pool_len(pool_tiles) * 4, BufferUse::Gpu);
        Ok(Self {
            pool_tiles,
            coarse,
            rows: device.create_buffer(pool_len(pool_tiles) * ROW_BYTES, BufferUse::Gpu),
            r: vector(),
            z: vector(),
            p: vector(),
            scratch: vector(),
            body: vector(),
            partials: device
                .create_buffer(u64::from(partial_count(pool_tiles)) * 4, BufferUse::Gpu),
            scalars: device.create_buffer(u64::from(2 * MAX_ITERATIONS) * 4, BufferUse::Gpu),
            progress: device.create_buffer(PROGRESS_BYTES, BufferUse::Gpu),
            flags: device.create_buffer(slots * 4, BufferUse::Gpu),
            lists: device.create_buffer(slots * 4, BufferUse::Gpu),
            bases,
            armed: armed(device, &pools),
            groups: armed_groups(&pools),
            gate: device.create_buffer(gate_bytes(pools.len()), BufferUse::Gpu),
            ranges: device.create_buffer(RANGES_BYTES, BufferUse::Gpu),
        })
    }
}

manifold_core::testkit_visible! {
/// Floats in the stop's record: four, then |r|∞ per iteration.
pub(crate) const PROGRESS_FLOATS: u32 = 4 + MAX_ITERATIONS;
}
const PROGRESS_BYTES: u64 = PROGRESS_FLOATS as u64 * 4;
/// Bytes of one indirect dispatch's three group counts.
const TRIPLE_BYTES: u64 = 12;
/// Range entries a solve's rounds take: two a round for the round-by-round
/// path, every round the cap allows (gpu_flip_pressure.wgsl ROUNDS), then one
/// per chunked execute from CHUNK_ENTRIES, never more than the rounds.
const RANGES_BYTES: u64 = 3 * MAX_ITERATIONS as u64 * GATED_RANGE_BYTES;
/// The first chunked execute's range entry (gpu_flip_pressure.wgsl CHUNK_ENTRIES).
pub(crate) const CHUNK_ENTRIES: u32 = 2 * MAX_ITERATIONS;
manifold_core::testkit_visible! {
/// Most rounds one execute runs, past the single executes: they double to
/// this, so a stop there leaves at most this less one guarded round running.
pub(crate) const ROUND_CHUNK: u32 = 64;
}
manifold_core::testkit_visible! {
/// Executes of one round each at the solve's start: every stop in the first
/// rounds, where solves stop, leaves no guarded round, which costs a whole
/// V-cycle, against an empty execute's few microseconds for each more.
pub(crate) const SINGLE_ROUNDS: u32 = 40;
}

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
static CHUNK_OVERRIDE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Test-only: the most rounds an execute runs while the guard lives; its
/// drop restores ROUND_CHUNK, so a panicking proof cannot leak the override
/// into the next test.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn set_round_chunk(chunk: u32) -> RoundChunkOverride {
    CHUNK_OVERRIDE.store(chunk, std::sync::atomic::Ordering::SeqCst);
    RoundChunkOverride
}

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
#[must_use = "the override ends when the guard drops"]
pub struct RoundChunkOverride;

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
impl Drop for RoundChunkOverride {
    fn drop(&mut self) {
        CHUNK_OVERRIDE.store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

fn round_chunk() -> u32 {
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    {
        let chunk = CHUNK_OVERRIDE.load(std::sync::atomic::Ordering::SeqCst);
        if chunk > 0 {
            return chunk;
        }
    }
    ROUND_CHUNK
}

/// Where each gated dispatch's groups sit in the gate, by triple: level l's
/// listed slots at l, then one group (the solve-live triple), then level l's
/// every slot (an in-round zero), then the body passes' two (written by the
/// arm, not held in `armed`).
#[derive(Clone, Copy)]
struct Slots {
    levels: usize,
}

impl Slots {
    fn level(self, l: usize) -> usize {
        l
    }
    fn single(self) -> usize {
        self.levels
    }
    /// Every cell of level l: a zero inside a round.
    fn dense(self, l: usize) -> usize {
        self.levels + 1 + l
    }
    /// The body impulse partial: the fine level's live workgroups by the bodies.
    fn body_partial(self) -> usize {
        2 * self.levels + 1
    }
    /// The body impulse finalize: one workgroup a body.
    fn body_finalize(self) -> usize {
        2 * self.levels + 2
    }
    /// The triples `armed` holds: every level's, the single one, every
    /// level's dense one.
    fn armed(self) -> u32 {
        2 * self.levels as u32 + 1
    }
    fn triples(self) -> u32 {
        2 * self.levels as u32 + 3
    }
}

fn gate_bytes(levels: usize) -> u64 {
    u64::from(Slots { levels }.triples()) * TRIPLE_BYTES
}

/// Partials a folded reduction over a pool of `pool_tiles` writes: two per
/// slot, held or not, however large the pool is.
fn partial_count(pool_tiles: u32) -> u32 {
    2 * pool_tiles
}

/// The gate's full group counts for `pools`, finest first, by triple:
/// every slot's two workgroups, then one group, then every level's cells.
fn armed_groups(pools: &[u32]) -> Vec<[u32; 3]> {
    let mut triples: Vec<[u32; 3]> = pools.iter().map(|&p| [partial_count(p), 1, 1]).collect();
    triples.push([1, 1, 1]);
    triples.extend(
        pools
            .iter()
            .map(|&p| groups(u64::from(p) * u64::from(TILE_CELLS))),
    );
    triples
}

/// [`armed_groups`] on the device; each prepare overwrites the level
/// triples with the live slot counts.
fn armed(device: &GpuDevice, pools: &[u32]) -> GpuBuffer {
    let words: Vec<u32> = armed_groups(pools).into_iter().flatten().collect();
    let buffer = device.create_buffer(words.len() as u64 * 4, BufferUse::Upload);
    // SAFETY: the buffer is shared, exactly `words` long, and no GPU work has
    // been encoded against it yet.
    unsafe { buffer.write(0, bytemuck::cast_slice(&words)) };
    buffer
}

fn bytes(params: &Params) -> GpuBinding<'_> {
    GpuBinding::Bytes {
        binding: 0,
        data: bytemuck::bytes_of(params),
    }
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

fn groups(threads: u64) -> [u32; 3] {
    [threads.div_ceil(u64::from(THREADS)) as u32, 1, 1]
}

/// A coarse level's links to the level below: its children rows (8 slots
/// below per slot) and, per slot below, its parent's slot.
#[derive(Clone, Copy)]
struct Links<'a> {
    children: &'a GpuBuffer,
    parents: &'a GpuBuffer,
}

/// One level's view for the V-cycle: the fine level reads the caller's water
/// and its rows and works in r and z; coarse levels' rows are plain.
#[derive(Clone, Copy)]
struct View<'a> {
    cell_size: f32,
    water: &'a GpuBuffer,
    faces: &'a GpuBuffer,
    rows: &'a GpuBuffer,
    rhs: &'a GpuBuffer,
    e: &'a GpuBuffer,
    /// 1 reads the rows' ghost diagonal, 0 the plain one.
    ghost: u32,
    /// The fold's partials: the sweep and residual passes reference them
    /// whether or not they fold, so every dispatch binds them.
    partials: &'a GpuBuffer,
    /// The level's first word in the slot flags and lists, and its gate
    /// triple.
    list_base: u32,
    level: u32,
    /// The table the level's fields are read through.
    tiles: &'a WorldTiles,
    /// A coarse level's links to the level below; None on the fine level.
    links: Option<Links<'a>>,
}

/// Level `level`'s view: the fine level is the caller's water and `fine`
/// tiles with the solver's r and z; a coarse level is its own.
fn view<'a>(b: &'a Buffers, water: &Water<'a>, fine: &'a WorldTiles, level: usize) -> View<'a> {
    match level {
        0 => View {
            cell_size: water.cell_size,
            water: water.water,
            faces: water.faces,
            rows: &b.rows,
            rhs: &b.r,
            e: &b.z,
            ghost: water.ghost().0,
            partials: &b.partials,
            list_base: 0,
            level: 0,
            tiles: fine,
            links: None,
        },
        l => {
            let c = &b.coarse[l - 1];
            View {
                cell_size: c.cell_size,
                water: &c.water,
                faces: &c.faces,
                rows: &c.rows,
                rhs: &c.rhs,
                e: &c.e,
                ghost: 0,
                partials: &b.partials,
                list_base: b.bases[l],
                level: l as u32,
                tiles: &c.tiles,
                links: Some(Links {
                    children: c
                        .tiles
                        .child_rows()
                        .expect("a parents table has children rows"),
                    parents: &c.parents,
                }),
            }
        }
    }
}

/// Which spare vector of the coarse levels above the gradient level a
/// transfer chain passes through: those levels' right-hand sides or
/// corrections, which a gradient started below them never reads.
#[derive(Clone, Copy)]
enum Spare {
    Rhs,
    E,
}

fn spare(b: &Buffers, level: usize, which: Spare) -> &GpuBuffer {
    let c = &b.coarse[level - 1];
    match which {
        Spare::Rhs => &c.rhs,
        Spare::E => &c.e,
    }
}

manifold_core::testkit_visible! {
/// One solve's inputs.
pub(crate) struct Solve<'a> {
    pub rhs: &'a GpuBuffer,
    /// The fine pressure, written with the answer. A solve on the fine
    /// level without bodies reads it first as its guess (the warm start):
    /// the step's buffer holds the last solve's answer, and a slot newly
    /// handed to a tile holds 0. The step's density solve writes the same
    /// buffer, so it starts from the tick's pressure and the next tick's
    /// pressure solve from its answer; any guess is valid, a closer one
    /// stops sooner.
    pub pressure: &'a GpuBuffer,
    pub stop: Stop,
    pub bodies: Option<(&'a BodyPasses, &'a Bodies<'a>)>,
    /// The V-cycle level the gradient runs on (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md
    /// section 11 (Solve Level)); 0 is the fine level. At most
    /// [`max_solve_level`].
    pub level: usize,
    /// Runs on a coarse level's right-hand side once the fine one is
    /// restricted down to it, before the gradient starts: the step's pocket
    /// mean at that level.
    pub coarse_rhs: Option<CoarseRhs<'a>>,
}
}

/// A pass over a coarse level's right-hand side: the buffer (a pooled field
/// of the level), the level's world tiles and its cell size.
pub(crate) type CoarseRhs<'a> = &'a dyn Fn(&mut GpuEncoder, &GpuBuffer, &WorldTiles, f32);

manifold_core::testkit_visible! {
#[derive(Default)]
pub(crate) struct PressureSolver {
    pipelines: Option<Pipelines>,
    buffers: Option<Buffers>,
    /// The round template every solve runs : the same chunked executes on every encoder path.
    templates: Option<GpuTemplateStore>,
    /// The GPU FLIP clock plan every pass reads (gpu_flip_pressure.wgsl
    /// slot_plan): zeros, always active, until the step sets its slot's.
    plan: Option<GpuBuffer>,
    /// The fine pool and cell size the levels were last built for, and
    /// whether the fine rows carry the ghost diagonal (prepare saw φ).
    prepared: Option<(u32, f32, bool)>,
}
}

impl PressureSolver {
    manifold_core::testkit_visible! {
        /// Build the solver's pipelines; the owning node calls this at install.
        pub(crate) fn prepare_pipelines(&mut self, device: &GpuDevice) {
            if self.pipelines.is_none() {
                self.pipelines = Some(Pipelines::new(device));
            }
            if self.templates.is_none() {
                self.templates = Some(GpuTemplateStore::new(device));
            }
            if self.plan.is_none() {
                let plan = device.create_buffer(48, BufferUse::Upload);
                plan.zero_fill();
                self.plan = Some(plan);
            }
        }
    }

    manifold_core::testkit_visible! {
        /// The clock plan of the slot the next prepare, solve and tally run in.
        /// An inactive slot's (live, no time to step) arms no gated pass, and
        /// every other pass returns at the top.
        pub(crate) fn set_clock_plan(&mut self, plan: &GpuBuffer) {
            self.plan = Some(plan.clone());
        }
    }

    manifold_core::testkit_visible! {
        /// Build the coarse levels from `fine`, the step's world tiles built
        /// earlier in this command buffer, and every level's active slots and
        /// operator rows for `water`. Every solve until the next prepare runs
        /// on this water and these tiles; a solve with φ needs a prepare that
        /// saw it. Allocates only when the fine pool changes.
        pub(crate) fn prepare(&mut self, device: &GpuDevice, enc: &mut GpuEncoder, water: &Water<'_>, fine: &WorldTiles) -> Result<(), String> {
            let pool_tiles = fine.pool_tiles();
            let phi = water.phi.map_or(u64::MAX, |phi| phi.size);
            if pool_len(pool_tiles) * 4 > water.water.size.min(phi) || pool_len(pool_tiles) * FACE_BYTES > water.faces.size {
                return Err(format!("a pool of {pool_tiles} tiles is larger than its water, distance or face arrays"));
            }
            if !(water.cell_size.is_finite() && water.cell_size > 0.0) {
                return Err("the cell size must be positive".into());
            }
            let pipes = self.pipelines.as_ref().expect("pressure pipelines built by prepare_pipelines at install");
            let plan = self.plan.as_ref().expect("the clock plan is set at install");
            if self.buffers.as_ref().is_none_or(|b| b.pool_tiles != pool_tiles) {
                self.buffers = Some(Buffers::new(device, pool_tiles)?);
            }
            let b = self.buffers.as_mut().expect("buffers sized");
            let mut h = water.cell_size;
            for level in &mut b.coarse {
                h *= 2.0;
                level.cell_size = h;
            }
            let b = &*b;
            let all_tiles = u32::from(super::gpu_flip_step::all_tiles());
            // A level's slot flags and active list from its touched mask (the
            // fine level's water, a coarse level's any-touched-child mask),
            // then its rows over the list.
            let assemble = |enc: &mut GpuEncoder, v: &View<'_>, touched: &GpuBuffer, phi: &GpuBuffer| {
                let params = Params { all_tiles, ghost: v.ghost, ..Params::at(v.cell_size).over(v) };
                enc.dispatch_compute(
                    &pipes.classify,
                    &[bytes(&params), buffer(1, touched), buffer(19, &b.flags), buffer(21, plan), buffer(22, v.tiles.table())],
                    groups(u64::from(v.tiles.pool_tiles()) * 32),
                    "gpu_flip.pressure.classify",
                );
                enc.dispatch_compute(
                    &pipes.lists,
                    &[bytes(&params), buffer(14, &b.armed), buffer(19, &b.flags), buffer(20, &b.lists), buffer(21, plan)],
                    [1, 1, 1],
                    "gpu_flip.pressure.lists",
                );
                enc.dispatch_compute_indirect(
                    &pipes.rows,
                    &[
                        bytes(&params),
                        buffer(1, v.water),
                        buffer(2, v.faces),
                        buffer(10, phi),
                        buffer(18, v.rows),
                        buffer(20, &b.lists),
                        buffer(21, plan),
                        buffer(22, v.tiles.table()),
                    ],
                    &b.armed,
                    u64::from(v.level) * TRIPLE_BYTES,
                    "gpu_flip.pressure.rows",
                );
            };
            assemble(enc, &view(b, water, fine, 0), water.water, water.ghost().1);
            for l in 1..=b.coarse.len() {
                let below = view(b, water, fine, l - 1);
                let at = view(b, water, fine, l);
                let c = &b.coarse[l - 1];
                let links = at.links.expect("a coarse level has links");
                c.tiles.encode_parents(enc, below.tiles, plan);
                let sides = Params::at(0.0).table_of(below.tiles).coarse_side(&c.tiles);
                enc.dispatch_compute(
                    &pipes.parents,
                    &[bytes(&sides), buffer(21, plan), buffer(22, below.tiles.table()), buffer(23, c.tiles.table()), buffer(25, links.parents)],
                    groups(u64::from(below.tiles.pool_tiles())),
                    "gpu_flip.pressure.parents",
                );
                // `level` is the fine side of this coarsening: at 0 the mask
                // has no solid kind and the kernel reads the faces for it.
                let params = Params { level: l as u32 - 1, ..sides };
                let below_touched = if l == 1 { water.water } else { &b.coarse[l - 2].touched };
                let held = &c.tiles.lists().by_rank;
                enc.dispatch_compute_indirect(
                    &pipes.coarsen_water,
                    &[
                        bytes(&params),
                        buffer(1, below.water),
                        buffer(2, below.faces),
                        buffer(4, below_touched),
                        buffer(5, &c.water),
                        buffer(6, &c.touched),
                        buffer(21, plan),
                        buffer(22, below.tiles.table()),
                        buffer(23, c.tiles.table()),
                        buffer(24, links.children),
                        buffer(26, held),
                    ],
                    c.tiles.args(),
                    allocated_args_offset(0),
                    "gpu_flip.pressure.coarsen_water",
                );
                enc.dispatch_compute_indirect(
                    &pipes.coarsen_faces,
                    &[
                        bytes(&params),
                        buffer(2, below.faces),
                        buffer(9, &c.faces),
                        buffer(21, plan),
                        buffer(23, c.tiles.table()),
                        buffer(24, links.children),
                        buffer(26, held),
                    ],
                    c.tiles.args(),
                    allocated_args_offset(0),
                    "gpu_flip.pressure.coarsen_faces",
                );
                assemble(enc, &at, &c.touched, &c.water);
            }
            self.prepared = Some((pool_tiles, water.cell_size, water.phi.is_some()));
            Ok(())
        }
    }

    manifold_core::testkit_visible! {
        /// Solve L p = `run.rhs` on the prepared water by conjugate gradient
        /// iterations, one V-cycle each; p into `run.pressure`. At level 0
        /// without bodies the iterations start from the pressure
        /// `run.pressure` holds on water with an open face (0 elsewhere), and
        /// a guess that already meets the stop runs none; otherwise they start
        /// from zero. `run.stop` says when it ends; the solve's record
        /// (iterations run, stopped or not) is in [`Self::progress`] once the
        /// GPU is done.
        /// `water` and `fine` must be what [`Self::prepare`] last saw. With
        /// bodies, the dynamic bodies sit inside the operator: every
        /// iteration's s = L p gains their ρh·G M⁻¹ Gᵀ p
        /// (`scripts/mgpcg_reference.py` `body_solve`). The V-cycle sees the
        /// water alone.
        ///
        /// At `run.level` k > 0 the gradient runs on level k: the right-hand side
        /// goes down by k restrictions (the V-cycle's Rᵀ, the first masked as
        /// init masks), the gradient and the V-cycle below it run on level k's
        /// rows and slots, and the pressure comes up by k prolongations into the
        /// zeroed fine pressure. The bodies stay fine: each direction is
        /// prolonged, their product taken there and restricted into s, so the
        /// coarse operator is L_k + Rᵀ B P.
        pub(crate) fn solve(&mut self, enc: &mut GpuEncoder, water: &Water<'_>, run: Solve<'_>, fine: &WorldTiles) -> Result<(), String> {
            let Solve { rhs, pressure, stop, bodies, level: k, coarse_rhs } = run;
            let Some((pool_tiles, cell_size, with_phi)) = self.prepared else {
                return Err("the solver was not prepared".into());
            };
            if (pool_tiles, cell_size) != (fine.pool_tiles(), water.cell_size) {
                return Err(format!("the solver was not prepared for a pool of {} tiles at this cell size", fine.pool_tiles()));
            }
            if water.phi.is_some() && !with_phi {
                return Err("the solver was prepared without the free surface's distance".into());
            }
            let iterations = stop.cap();
            if !(1..=MAX_ITERATIONS).contains(&iterations) {
                return Err(format!("iterations must be 1 to {MAX_ITERATIONS}, not {iterations}"));
            }
            if pool_len(pool_tiles) * 4 > rhs.size.min(pressure.size) {
                return Err(format!("a pool of {pool_tiles} tiles is larger than the right-hand side or the pressure"));
            }
            if let Some(reason) = level_refusal(pool_tiles, k) {
                return Err(reason);
            }
            let (Some(pipes), Some(b), Some(plan)) = (self.pipelines.as_ref(), self.buffers.as_ref(), self.plan.as_ref()) else {
                return Err("the solver was not prepared".into());
            };
            let slots = Slots { levels: b.coarse.len() + 1 };
            // Inside a round every pass reads its live counts from the gate, which
            // the stop zeroes, and the singleton passes check the solve-live
            // triple: a round that runs after the stop writes nothing.
            let g = Gate {
                buffer: &b.gate,
                groups: &b.groups,
                slots,
                gated: true,
                tiles: [&b.gate, &b.flags, &b.lists],
                plan,
                live: slots.single() as u32,
            };
            // The gate is armed just below, so the prelude always runs full:
            // plain dispatches, which record with the rest of the step.
            let plain = Gate { gated: false, tiles: [&b.armed, &b.flags, &b.lists], live: 0, ..g };
            // The gradient's level: its r and z are the solver's, its x and
            // right-hand side the caller's at level 0, level k's own above.
            let top = View { rhs: &b.r, e: &b.z, ..view(b, water, fine, k) };
            let (rhs_k, x): (&GpuBuffer, &GpuBuffer) = if k == 0 { (rhs, pressure) } else { (spare(b, k, Spare::Rhs), spare(b, k, Spare::E)) };
            let chain = Chain { pipes, b, water, fine, g: plain, k };
            // Each round is one replayed segment: the arm writes every round's
            // range entries as live and the stop zeroes the rounds after it,
            // with the gate. The body passes ride the gate, so a stopped
            // solve's later rounds run none of them.
            let round = match bodies {
                None => BodyRound::Without,
                Some(_) if k == 0 => BodyRound::Gated,
                Some(_) => BodyRound::GatedCoarse(k),
            };
            let (before, after) = round_commands(b.coarse.len() - k, round);
            let body_count = match (round, bodies) {
                (BodyRound::Gated | BodyRound::GatedCoarse(_), Some((_, bodies))) => bodies.count.max(1),
                _ => 0,
            };
            let grad = Params { triples: slots.triples(), tolerance: stop.tolerance(), ..Params::at(top.cell_size).over(&top) };
            // The arm's `mode` is the rounds and `live` the chunk, for the chunked
            // executes' range entries.
            let arm = Params { color: before, slot: after, armed_triples: slots.armed(), bodies: body_count, mode: iterations, live: round_chunk(), singles: SINGLE_ROUNDS, ..grad };
            let body_gate = BodyGate {
                buffer: &b.gate,
                partial: slots.body_partial() as u64 * TRIPLE_BYTES,
                finalize: slots.body_finalize() as u64 * TRIPLE_BYTES,
                product: slots.level(0) as u64 * TRIPLE_BYTES,
                live: g.live,
            };
            let r = RoundPasses { pipes, b, water, fine, g, top, grad, x, k, bodies: bodies.is_some() };
            // Every round encodes the same bytes (the round index is the GPU's
            // progress[1]), so one walked round is the template the GPU runs once
            // per round, in executes of one round for the first SINGLE_ROUNDS,
            // then 1, 2, 4, … up to ROUND_CHUNK rounds, execute j by range
            // entry CHUNK_ENTRIES + j, on every encoder path.
            // It is prepared before anything of the solve encodes, so a refused
            // template leaves the command buffer untouched. On the gate the body
            // product runs in live rounds only; at a coarse level its chain (the
            // direction down to the fine level, the product back up) rides the
            // gate too.
            debug_assert_eq!(after, 0, "a gated round is one segment");
            let store = self.templates.as_mut().ok_or("the solver was not prepared")?;
            let at = TemplateRanges { ranges: &b.ranges, first: CHUNK_ENTRIES, stride: 1, chunk: round_chunk(), singles: SINGLE_ROUNDS };
            let ticket = enc.prepare_template(store, at, before, iterations, |recorder| {
                let mut sink = Round { recorder, refused: None };
                r.head(&mut sink);
                match (round, bodies) {
                    (BodyRound::Gated, Some((passes, bodies))) => {
                        passes.apply_gated(&mut sink, bodies, g.tiles, &b.p, &b.scratch, &body_gate)?;
                    }
                    (BodyRound::GatedCoarse(_), Some((passes, bodies))) => {
                        let inside = Chain { g, ..chain };
                        inside.prolong_down(&mut sink, &b.p, pressure, Spare::E);
                        zero_in(&mut sink, pipes, g, &view(b, water, fine, 0), &b.body);
                        passes.apply_gated(&mut sink, bodies, g.tiles, pressure, &b.body, &body_gate)?;
                        inside.restrict_down(&mut sink, &b.body, &b.scratch, 0, RESTRICT_ADD);
                    }
                    _ => {}
                }
                r.tail(&mut sink);
                sink.refused.map_or(Ok(()), Err)
            })?;
            if k > 0 {
                chain.restrict_down(enc, rhs, rhs_k, RESTRICT_MASK, 0);
                if let Some(hook) = coarse_rhs {
                    hook(enc, rhs_k, top.tiles, top.cell_size);
                }
            }
            enc.dispatch_compute(
                &pipes.arm,
                &[bytes(&arm), buffer(12, &b.gate), buffer(14, &b.armed), buffer(15, &b.ranges), buffer(21, plan)],
                [1, 1, 1],
                "gpu_flip.pressure.arm",
            );
            // The warm start: the fine pressure is the guess. Above level 0 x is
            // the level's own correction, which starts from zero; with bodies
            // r0 would need their product on the guess, so they start from zero.
            let warm = k == 0 && bodies.is_none();
            plain.dispatch(
                enc,
                &pipes.init,
                &[
                    bytes(&Params { mode: REDUCE | if warm { WARM } else { 0 }, ..grad }),
                    buffer(1, top.water),
                    buffer(2, top.faces),
                    buffer(3, rhs_k),
                    buffer(5, &b.r),
                    buffer(6, x),
                    buffer(16, &b.partials),
                    buffer(22, top.tiles.table()),
                ],
                slots.level(k),
                "gpu_flip.pressure.init",
            );
            check(enc, pipes, b, plain, &Params { mode: 1, ..grad });
            if warm {
                // r = f − L x0; on the engine's stop its |r0|∞ folds for a check
                // that ends the solve at once when the guess already meets it.
                let converged = stop.tolerance() >= 0.0;
                plain.dispatch_listed(
                    enc,
                    &pipes.warm,
                    &[
                        bytes(&Params { mode: if converged { REDUCE } else { 0 }, ghost: top.ghost, ..grad }),
                        buffer(1, top.water),
                        buffer(3, rhs),
                        // The stencil names `aux` for the passes that read
                        // it; warm never does, and Vulkan refuses a named
                        // binding left out.
                        buffer(4, rhs),
                        buffer(5, x),
                        buffer(6, &b.r),
                        buffer(11, &b.progress),
                        buffer(16, &b.partials),
                        buffer(17, top.rows),
                        buffer(22, top.tiles.table()),
                    ],
                    slots.level(0),
                    "gpu_flip.pressure.warm",
                );
                if converged {
                    check(enc, pipes, b, plain, &Params { mode: WARM, ..grad });
                }
            }
            let store = self.templates.as_mut().ok_or("the solver was not prepared")?;
            enc.execute_template(store, ticket)?;
            if k > 0 {
                chain.prolong_down(enc, x, pressure, Spare::E);
            }
            Ok(())
        }
    }

    manifold_core::testkit_visible! {
        /// The fine level's slot buffers after a prepare (the gate triples,
        /// the flags, the lists of active slots; the fine level's words start
        /// at 0 in each), for the body passes that run outside the solve.
        pub(crate) fn tiles(&self) -> Result<[&GpuBuffer; 3], String> {
            let b = self.buffers.as_ref().ok_or("the solver was not prepared")?;
            debug_assert_eq!(b.bases[0], 0, "the fine level's flags and lists start at word 0");
            Ok([&b.armed, &b.flags, &b.lists])
        }
    }

    /// Coarse level `level`'s world tiles after a prepare (level 1 and
    /// deeper): the table its fields are read through, its held slots, and
    /// in its counts' exhausted word whether its pool ran short.
    pub(crate) fn level_tiles(&self, level: usize) -> Result<&WorldTiles, String> {
        let b = self.buffers.as_ref().ok_or("the solver was not prepared")?;
        level
            .checked_sub(1)
            .and_then(|i| b.coarse.get(i))
            .map(|c| &c.tiles)
            .ok_or_else(|| format!("the solver has no coarse level {level}"))
    }

    /// What the round template store did over the solver's life.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn template_stats(&self) -> manifold_gpu::GpuTemplateStats {
        self.templates
            .as_ref()
            .map(|t| t.stats())
            .unwrap_or_default()
    }

    /// The work vectors a round writes: r, z, p, the scratch and the body
    /// product, for proofs that compare every output of a solve.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn work_vectors(&self) -> Option<[&GpuBuffer; 5]> {
        self.buffers
            .as_ref()
            .map(|b| [&b.r, &b.z, &b.p, &b.scratch, &b.body])
    }

    /// The gated range entries: per-round from 0, chunked executes from
    /// [`CHUNK_ENTRIES`], `{location, length}` u32 pairs.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn ranges(&self) -> Option<&GpuBuffer> {
        self.buffers.as_ref().map(|b| &b.ranges)
    }

    /// The last solve's record: |f|∞, iterations run, 1.0 when it stopped by
    /// the tolerance, then |r|∞ per iteration; [`PROGRESS_FLOATS`] floats.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn progress(&self) -> Option<&GpuBuffer> {
        self.buffers.as_ref().map(|b| &b.progress)
    }

    /// Coarse level `level`'s parents after a prepare: per slot of the level
    /// below, its parent's slot (WT_NONE when it has none).
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn level_parents(&self, level: usize) -> Option<&GpuBuffer> {
        let b = self.buffers.as_ref()?;
        level
            .checked_sub(1)
            .and_then(|i| b.coarse.get(i))
            .map(|c| &c.parents)
    }

    /// Test-only: NaN into every cell outside each level's active slots in
    /// the vectors a solve reads inside them (z, p, the residual scratch and
    /// the body product, on the gradient's `level`; each coarse level's
    /// right-hand side and correction) and into the gradient level's
    /// inactive partials, after a prepare. A solve that reads past its
    /// lists shows in its pressure.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn poison(
        &self,
        enc: &mut GpuEncoder,
        water: &Water<'_>,
        level: usize,
        fine: &WorldTiles,
    ) {
        let (Some(pipes), Some(b)) = (self.pipelines.as_ref(), self.buffers.as_ref()) else {
            panic!("the solver was not prepared");
        };
        let top = view(b, water, fine, level);
        let mut targets = vec![
            (top, &b.z),
            (top, &b.p),
            (top, &b.scratch),
            (view(b, water, fine, 0), &b.body),
        ];
        for l in 1..=b.coarse.len() {
            let v = view(b, water, fine, l);
            targets.push((v, v.rhs));
            targets.push((v, v.e));
        }
        for (v, out) in targets {
            let params = Params {
                color: u32::from(v.level == top.level),
                ..Params::at(0.0).over(&v)
            };
            enc.dispatch_compute(
                &pipes.poison,
                &[
                    bytes(&params),
                    buffer(5, out),
                    buffer(16, &b.partials),
                    buffer(19, &b.flags),
                ],
                [partial_count(v.tiles.pool_tiles()), 1, 1],
                "gpu_flip.pressure.poison",
            );
        }
    }

    /// Add the last solve, run with `stop`, to the tick's solver words at
    /// `offset` bytes in `into` (gpu_flip_pressure.wgsl tally_main): its
    /// iterations to `word`, and 1 to word 2 when it reached its cap without
    /// meeting the tolerance. `clear` zeroes the words first.
    pub(crate) fn tally(
        &self,
        enc: &mut GpuEncoder,
        stop: Stop,
        into: &GpuBuffer,
        offset: u64,
        word: u32,
        clear: bool,
    ) -> Result<(), String> {
        let (Some(pipes), Some(b), Some(plan)) = (
            self.pipelines.as_ref(),
            self.buffers.as_ref(),
            self.plan.as_ref(),
        ) else {
            return Err("the solver was not prepared".into());
        };
        let params = Params {
            slot: word,
            mode: u32::from(clear),
            tolerance: stop.tolerance(),
            ..Params::default()
        };
        enc.dispatch_compute(
            &pipes.tally,
            &[
                bytes(&params),
                buffer(11, &b.progress),
                GpuBinding::Buffer {
                    binding: 13,
                    buffer: into,
                    offset,
                },
                buffer(21, plan),
            ],
            [1, 1, 1],
            "gpu_flip.pressure.tally",
        );
        Ok(())
    }
}

manifold_core::testkit_visible! {
/// When a solve ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Stop {
    /// Exactly this many iterations.
    Fixed(u32),
    /// The engine's stop (gpu_flip_pressure.wgsl check_main), after at most
    /// this many iterations.
    Converged(u32),
}
}

impl Stop {
    fn cap(self) -> u32 {
        match self {
            Stop::Fixed(n) | Stop::Converged(n) => n,
        }
    }

    fn tolerance(self) -> f32 {
        match self {
            Stop::Fixed(_) => -1.0,
            Stop::Converged(_) => TOLERANCE,
        }
    }
}

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
impl PressureSolver {
    /// Copies the last solve's scalars (iteration k's r·z at 2k, p·s at
    /// 2k + 1) into `into`, a shared buffer of 2 · MAX_ITERATIONS floats.
    pub fn copy_scalars(&self, enc: &mut GpuEncoder, into: &GpuBuffer) {
        let b = self.buffers.as_ref().expect("the solver was prepared");
        enc.copy_buffer_to_buffer(&b.scalars, into, b.scalars.size);
    }

    /// Fills the scalars and the stop's record from `sentinel` (at least as
    /// large as either), so storage a solve leaves unwritten holds known bits.
    pub fn seed_records(&self, enc: &mut GpuEncoder, sentinel: &GpuBuffer) {
        let b = self.buffers.as_ref().expect("the solver was prepared");
        enc.copy_buffer_to_buffer(sentinel, &b.scalars, b.scalars.size);
        enc.copy_buffer_to_buffer(sentinel, &b.progress, b.progress.size);
    }

    /// Copies the fine level's rows (`ROW_BYTES` a cell, gpu_flip_pressure.wgsl
    /// Row) into `into`, a shared buffer of the fine pool's cells × `ROW_BYTES`.
    #[cfg(test)]
    pub(crate) fn copy_rows(&self, enc: &mut GpuEncoder, into: &GpuBuffer) {
        let b = self.buffers.as_ref().expect("the solver was prepared");
        enc.copy_buffer_to_buffer(&b.rows, into, b.rows.size);
    }
}

/// Bytes of one cell's row, for the proofs.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) const ROW_FLOATS: usize = ROW_BYTES as usize / 4;

/// A solve's dispatches run on the gate's group counts, which the stop
/// zeroes: as indirect dispatches when encoded directly, and inside a
/// replayed round as recorded dispatches of the full counts, which the
/// round's range entry switches off with the gate. Ungated, a dispatch is
/// plain at the full counts.
#[derive(Clone, Copy)]
struct Gate<'a> {
    buffer: &'a GpuBuffer,
    groups: &'a [[u32; 3]],
    slots: Slots,
    gated: bool,
    /// The slot lists every dispatch reads its cells and partials through:
    /// the live counts, the flags and the lists.
    tiles: [&'a GpuBuffer; 3],
    /// The clock plan (gpu_flip_pressure.wgsl slot_plan).
    plan: &'a GpuBuffer,
    /// The solve-live triple the singleton passes check (Params live); 0
    /// outside rounds.
    live: u32,
}

/// The most bindings one solve dispatch names, the slot buffers and the
/// clock plan included.
const MAX_BINDINGS: usize = 14;

manifold_core::testkit_visible! {
/// Where a solve's dispatches go: the encoder, or a round template's
/// recorder, which takes gated dispatches only.
pub(crate) trait Sink {
    fn gated(&mut self, pipeline: &GpuComputePipeline, bindings: &[GpuBinding], groups: [u32; 3], gate: &GpuBuffer, offset: u64, label: &str);
    fn plain(&mut self, pipeline: &GpuComputePipeline, bindings: &[GpuBinding], groups: [u32; 3], label: &str);
}
}

impl Sink for GpuEncoder {
    fn gated(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        groups: [u32; 3],
        gate: &GpuBuffer,
        offset: u64,
        label: &str,
    ) {
        self.dispatch_compute_gated(pipeline, bindings, groups, gate, offset, label);
    }

    fn plain(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        groups: [u32; 3],
        label: &str,
    ) {
        self.dispatch_compute(pipeline, bindings, groups, label);
    }
}

/// Metal binds only what a dispatch names: a binding the entry point
/// references and the dispatch leaves out reads whatever its slot last held.
/// Checked on the zero, whose stop check reads `armed`; not on every pass:
/// smooth references binding 4 on sources it never runs with.
fn debug_assert_bound(pipeline: &GpuComputePipeline, bindings: &[GpuBinding], label: &str) {
    debug_assert!(
        pipeline.unbound_binding(bindings).is_none(),
        "{label} leaves binding {:?} unbound",
        pipeline.unbound_binding(bindings)
    );
}

/// A round template's body. A plain dispatch has no place in a template (the
/// GPU repeats the round, and only gated dispatches stop with the solve), so
/// it is refused, and the refusal fails the solve.
struct Round<'r, 'e> {
    recorder: &'r mut GatedRecorder<'e>,
    refused: Option<String>,
}

impl Sink for Round<'_, '_> {
    fn gated(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        groups: [u32; 3],
        gate: &GpuBuffer,
        offset: u64,
        label: &str,
    ) {
        self.recorder
            .dispatch_gated(pipeline, bindings, groups, gate, offset, label);
    }

    fn plain(&mut self, _: &GpuComputePipeline, _: &[GpuBinding], _: [u32; 3], label: &str) {
        self.refused.get_or_insert_with(|| {
            format!("the pressure round template refuses the ungated dispatch {label}")
        });
    }
}

/// The most workgroups a listed pass launches: enough to fill the GPU, each
/// running list workgroups in turn past them (gpu_flip_pressure.wgsl
/// `listed`), so a recorded round costs what the active slots do, not the
/// pool.
const LISTED_GROUPS: u32 = 512;

impl Gate<'_> {
    fn dispatch<S: Sink>(
        self,
        enc: &mut S,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        triple: usize,
        label: &str,
    ) {
        self.launch(enc, pipeline, bindings, triple, self.groups[triple], label);
    }

    /// A pass over the active list of level `triple` (a `_main` that turns
    /// over its list), launched with at most [`LISTED_GROUPS`].
    fn dispatch_listed<S: Sink>(
        self,
        enc: &mut S,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        triple: usize,
        label: &str,
    ) {
        let [groups, y, z] = self.groups[triple];
        let launched = [groups.min(LISTED_GROUPS), y, z];
        self.launch(enc, pipeline, bindings, triple, launched, label);
    }

    fn launch<S: Sink>(
        self,
        enc: &mut S,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        triple: usize,
        groups: [u32; 3],
        label: &str,
    ) {
        let mut all: arrayvec::ArrayVec<GpuBinding, MAX_BINDINGS> = bindings
            .iter()
            .map(|b| match *b {
                GpuBinding::Buffer {
                    binding,
                    buffer,
                    offset,
                } => GpuBinding::Buffer {
                    binding,
                    buffer,
                    offset,
                },
                GpuBinding::Texture { binding, texture } => {
                    GpuBinding::Texture { binding, texture }
                }
                GpuBinding::Sampler { binding, sampler } => {
                    GpuBinding::Sampler { binding, sampler }
                }
                GpuBinding::Bytes { binding, data } => GpuBinding::Bytes { binding, data },
            })
            .collect();
        all.push(buffer(14, self.tiles[0]));
        all.push(buffer(19, self.tiles[1]));
        all.push(buffer(20, self.tiles[2]));
        all.push(buffer(21, self.plan));
        if self.gated {
            enc.gated(
                pipeline,
                &all,
                groups,
                self.buffer,
                triple as u64 * TRIPLE_BYTES,
                label,
            );
        } else {
            enc.plain(pipeline, &all, groups, label);
        }
    }
}

/// One conjugate gradient round's passes around the body product: the
/// head (V-cycle, r·z, direction, operator) and the tail (p·s with bodies,
/// update, check).
#[derive(Clone, Copy)]
struct RoundPasses<'a> {
    pipes: &'a Pipelines,
    b: &'a Buffers,
    water: &'a Water<'a>,
    fine: &'a WorldTiles,
    g: Gate<'a>,
    top: View<'a>,
    /// The gradient level's params.
    grad: Params,
    x: &'a GpuBuffer,
    k: usize,
    bodies: bool,
}

impl RoundPasses<'_> {
    fn head<S: Sink>(&self, enc: &mut S) {
        let (pipes, b, g, top, grad) = (self.pipes, self.b, self.g, &self.top, self.grad);
        v_cycle(enc, pipes, b, self.water, self.fine, g, top);
        dot_finalize(enc, pipes, b, g, top, 0);
        g.dispatch_listed(
            enc,
            &pipes.direction,
            &[
                bytes(&grad),
                buffer(3, &b.z),
                buffer(5, &b.p),
                buffer(7, &b.scalars),
                buffer(11, &b.progress),
            ],
            g.slots.level(self.k),
            "gpu_flip.pressure.direction",
        );
        // With no bodies s is final here, so the pass folds p · s.
        let apply = Params {
            mode: 1 | if self.bodies { 0 } else { REDUCE },
            ghost: top.ghost,
            ..grad
        };
        g.dispatch_listed(
            enc,
            &pipes.residual,
            &[
                bytes(&apply),
                buffer(1, top.water),
                buffer(3, &b.p),
                buffer(4, &b.p),
                buffer(5, &b.scratch),
                buffer(16, &b.partials),
                buffer(17, top.rows),
                buffer(22, top.tiles.table()),
            ],
            g.slots.level(self.k),
            "gpu_flip.pressure.apply",
        );
    }

    fn tail<S: Sink>(&self, enc: &mut S) {
        let (pipes, b, g, top, grad) = (self.pipes, self.b, self.g, &self.top, self.grad);
        if self.bodies {
            g.dispatch_listed(
                enc,
                &pipes.dot_partial,
                &[
                    bytes(&grad),
                    buffer(3, &b.p),
                    buffer(4, &b.scratch),
                    buffer(16, &b.partials),
                ],
                g.slots.level(self.k),
                "gpu_flip.pressure.dot_partial",
            );
        }
        dot_finalize(enc, pipes, b, g, top, 1);
        g.dispatch_listed(
            enc,
            &pipes.update,
            &[
                bytes(&Params {
                    mode: REDUCE,
                    ..grad
                }),
                buffer(3, &b.p),
                buffer(4, &b.scratch),
                buffer(5, self.x),
                buffer(6, &b.r),
                buffer(7, &b.scalars),
                buffer(16, &b.partials),
                buffer(11, &b.progress),
            ],
            g.slots.level(self.k),
            "gpu_flip.pressure.update",
        );
        check(enc, pipes, b, g, &grad);
    }
}

/// z = V(r): one V-cycle for L e = r from zero, from the gradient's level
/// `top` down. The last sweep at `top` folds r · z into the partials.
fn v_cycle<'a, S: Sink>(
    enc: &mut S,
    pipes: &Pipelines,
    b: &'a Buffers,
    water: &Water<'a>,
    fine: &'a WorldTiles,
    g: Gate<'_>,
    top: &View<'a>,
) {
    let start = top.level as usize;
    let at = |level: usize| -> View<'a> {
        if level == start {
            *top
        } else {
            view(b, water, fine, level)
        }
    };
    let last = b.coarse.len();
    for level in start..last {
        let v = at(level);
        let coarse = at(level + 1);
        for round in 0..SMOOTH_ROUNDS {
            for color in [0, 1] {
                let sweep = if round == 0 && color == 0 {
                    Sweep::FromZero
                } else {
                    Sweep::Plain
                };
                smooth(enc, pipes, &v, color, sweep, g);
            }
        }
        g.dispatch_listed(
            enc,
            &pipes.residual,
            &[
                bytes(&Params {
                    ghost: v.ghost,
                    ..Params::at(v.cell_size).over(&v)
                }),
                buffer(1, v.water),
                buffer(3, v.rhs),
                buffer(4, v.e),
                buffer(5, &b.scratch),
                buffer(16, v.partials),
                buffer(17, v.rows),
                buffer(22, v.tiles.table()),
            ],
            g.slots.level(level),
            "gpu_flip.pressure.residual",
        );
        restrict(enc, pipes, g, &v, &coarse, &b.scratch, coarse.rhs, 0);
    }
    // The coarsest level: red then black from zero, then black then red,
    // COARSEST_SWEEPS of each, a symmetric approximate solve in one workgroup.
    let v = at(last);
    g.dispatch(
        enc,
        &pipes.coarsest,
        &[
            bytes(&Params {
                color: COARSEST_SWEEPS,
                ghost: v.ghost,
                ..Params::at(v.cell_size).over(&v)
            }),
            buffer(1, v.water),
            buffer(3, v.rhs),
            buffer(4, v.rhs),
            buffer(5, v.e),
            buffer(17, v.rows),
            buffer(22, v.tiles.table()),
        ],
        g.slots.single(),
        "gpu_flip.pressure.coarsest",
    );
    for level in (start..last).rev() {
        let v = at(level);
        let coarse = at(level + 1);
        prolong(enc, pipes, g, &v, &coarse, coarse.e, v.e);
        for round in 0..SMOOTH_ROUNDS {
            for color in [1, 0] {
                let last = level == start && round == SMOOTH_ROUNDS - 1 && color == 0;
                let sweep = if last { Sweep::Fold } else { Sweep::Plain };
                smooth(enc, pipes, &v, color, sweep, g);
            }
        }
    }
}

/// `src` on `fine` restricted into `out` on `coarse`, over the coarse
/// level's list; `mode` is the restrict bits.
fn restrict<S: Sink>(
    enc: &mut S,
    pipes: &Pipelines,
    g: Gate<'_>,
    fine: &View<'_>,
    coarse: &View<'_>,
    src: &GpuBuffer,
    out: &GpuBuffer,
    mode: u32,
) {
    let links = coarse
        .links
        .expect("a transfer's coarse side is a coarse level");
    // The list is the coarse level's, the lookups the fine one's.
    let params = Params {
        mode,
        below_base: fine.list_base,
        ..Params::at(fine.cell_size)
            .table_of(fine.tiles)
            .coarse_side(coarse.tiles)
            .listed_on(coarse)
    };
    g.dispatch_listed(
        enc,
        &pipes.restrict,
        &[
            bytes(&params),
            buffer(1, fine.water),
            buffer(2, fine.faces),
            buffer(3, src),
            buffer(5, out),
            buffer(8, coarse.water),
            buffer(22, fine.tiles.table()),
            buffer(23, coarse.tiles.table()),
            buffer(24, links.children),
        ],
        g.slots.level(coarse.level as usize),
        "gpu_flip.pressure.restrict",
    );
}

/// `src` on `coarse` prolonged and added into `out` on `fine`'s water,
/// over the fine level's list.
fn prolong<S: Sink>(
    enc: &mut S,
    pipes: &Pipelines,
    g: Gate<'_>,
    fine: &View<'_>,
    coarse: &View<'_>,
    src: &GpuBuffer,
    out: &GpuBuffer,
) {
    let links = coarse
        .links
        .expect("a transfer's coarse side is a coarse level");
    let params = Params::at(fine.cell_size)
        .over(fine)
        .coarse_side(coarse.tiles);
    g.dispatch_listed(
        enc,
        &pipes.prolong,
        &[
            bytes(&params),
            buffer(1, fine.water),
            buffer(3, src),
            buffer(5, out),
            buffer(8, coarse.water),
            buffer(22, fine.tiles.table()),
            buffer(23, coarse.tiles.table()),
            buffer(25, links.parents),
        ],
        g.slots.level(fine.level as usize),
        "gpu_flip.pressure.prolong",
    );
}

/// Every held slot of `out` on level `v` to 0: a dispatch, so it records
/// and labels with the solver's passes.
fn zero<S: Sink>(
    enc: &mut S,
    pipes: &Pipelines,
    v: &View<'_>,
    out: &GpuBuffer,
    armed: &GpuBuffer,
    plan: &GpuBuffer,
) {
    // `armed` with `live` 0: the shader's stop check reads it and never stops.
    let params = Params::at(0.0).table_of(v.tiles);
    let bindings = [
        bytes(&params),
        buffer(5, out),
        buffer(14, armed),
        buffer(21, plan),
        buffer(22, v.tiles.table()),
    ];
    debug_assert_bound(&pipes.zero, &bindings, "gpu_flip.pressure.zero");
    enc.plain(
        &pipes.zero,
        &bindings,
        [partial_count(v.tiles.pool_tiles()), 1, 1],
        "gpu_flip.pressure.zero",
    );
}

/// [`zero`] of level `v` on `g`: inside a round, gated on the level's
/// dense triple and skipped once the solve stopped; outside, plain.
fn zero_in<S: Sink>(enc: &mut S, pipes: &Pipelines, g: Gate<'_>, v: &View<'_>, out: &GpuBuffer) {
    if !g.gated {
        zero(enc, pipes, v, out, g.tiles[0], g.plan);
        return;
    }
    let params = Params {
        live: g.live,
        ..Params::at(0.0).table_of(v.tiles)
    };
    g.dispatch(
        enc,
        &pipes.zero,
        &[bytes(&params), buffer(5, out), buffer(22, v.tiles.table())],
        g.slots.dense(v.level as usize),
        "gpu_flip.pressure.zero",
    );
}

/// A transfer chain between the fine level and the gradient's level k,
/// ungated: the fine right-hand side down, a body direction down and its
/// product up, the pressure up.
#[derive(Clone, Copy)]
struct Chain<'a> {
    pipes: &'a Pipelines,
    b: &'a Buffers,
    water: &'a Water<'a>,
    fine: &'a WorldTiles,
    g: Gate<'a>,
    k: usize,
}

impl<'a> Chain<'a> {
    fn view(&self, level: usize) -> View<'a> {
        view(self.b, self.water, self.fine, level)
    }

    /// `src` on the fine level restricted k times into `out` on level k,
    /// through the spare right-hand sides between; `first` and `last` are
    /// the restrict bits of the first and last step.
    fn restrict_down<S: Sink>(
        self,
        enc: &mut S,
        src: &GpuBuffer,
        out: &GpuBuffer,
        first: u32,
        last: u32,
    ) {
        let mut from = src;
        for j in 1..=self.k {
            let to = if j == self.k {
                out
            } else {
                spare(self.b, j, Spare::Rhs)
            };
            let mode = if j == 1 { first } else { 0 } | if j == self.k { last } else { 0 };
            restrict(
                enc,
                self.pipes,
                self.g,
                &self.view(j - 1),
                &self.view(j),
                from,
                to,
                mode,
            );
            from = to;
        }
    }

    /// `src` on level k prolonged k times into `out` on the fine level,
    /// through the spare vectors `which` between; every target is zeroed
    /// first, so each holds its level's water values and 0 elsewhere.
    fn prolong_down<S: Sink>(self, enc: &mut S, src: &GpuBuffer, out: &GpuBuffer, which: Spare) {
        let mut from = src;
        for j in (0..self.k).rev() {
            let fine = self.view(j);
            let to = if j == 0 { out } else { spare(self.b, j, which) };
            zero_in(enc, self.pipes, self.g, &fine, to);
            prolong(enc, self.pipes, self.g, &fine, &self.view(j + 1), from, to);
            from = to;
        }
    }
}

/// What a sweep does besides relaxing: the first of a level starts from
/// zero; the solve's last one at the gradient's level folds rhs · e into
/// the partials.
#[derive(Clone, Copy)]
enum Sweep {
    Plain,
    FromZero,
    Fold,
}

/// One red-black sweep of `color` at level `v`.
fn smooth<S: Sink>(
    enc: &mut S,
    pipes: &Pipelines,
    v: &View<'_>,
    color: u32,
    sweep: Sweep,
    g: Gate<'_>,
) {
    let mode = match sweep {
        Sweep::Plain => 0,
        Sweep::FromZero => 1,
        Sweep::Fold => REDUCE,
    };
    let params = Params {
        color,
        mode,
        ghost: v.ghost,
        ..Params::at(v.cell_size).over(v)
    };
    g.dispatch_listed(
        enc,
        &pipes.smooth,
        &[
            bytes(&params),
            buffer(1, v.water),
            buffer(3, v.rhs),
            buffer(4, v.rhs),
            buffer(5, v.e),
            buffer(16, v.partials),
            buffer(17, v.rows),
            buffer(22, v.tiles.table()),
        ],
        g.slots.level(v.level as usize),
        "gpu_flip.pressure.smooth",
    );
}

/// The folded partials of a dot product over the gradient's level summed,
/// in a fixed order, into this round's scalar `slot`: 0 r·z, 1 p·s.
fn dot_finalize<S: Sink>(
    enc: &mut S,
    pipes: &Pipelines,
    b: &Buffers,
    g: Gate<'_>,
    top: &View<'_>,
    slot: u32,
) {
    let params = Params {
        color: partial_count(top.tiles.pool_tiles()),
        slot,
        live: g.live,
        ..Params::at(0.0).over(top)
    };
    g.dispatch(
        enc,
        &pipes.dot_finalize,
        &[
            bytes(&params),
            buffer(7, &b.scalars),
            buffer(11, &b.progress),
            buffer(16, &b.partials),
        ],
        g.slots.single(),
        "gpu_flip.pressure.dot_finalize",
    );
}

/// The folded partials' max, |r|∞, into the stop's record and the stop
/// test: the start with `mode` 1, the warm start's with [`WARM`], else after
/// the iteration the record counts. `step` is the gradient level's params.
fn check<S: Sink>(enc: &mut S, pipes: &Pipelines, b: &Buffers, g: Gate<'_>, step: &Params) {
    let params = Params {
        color: partial_count(step.pool_tiles),
        live: g.live,
        slot: round_chunk(),
        singles: SINGLE_ROUNDS,
        ..*step
    };
    g.dispatch(
        enc,
        &pipes.check,
        &[
            bytes(&params),
            buffer(11, &b.progress),
            buffer(12, &b.gate),
            buffer(15, &b.ranges),
            buffer(16, &b.partials),
        ],
        g.slots.single(),
        "gpu_flip.pressure.check",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The depth rule on a cube of ⌈P^⅓⌉ tiles, each level's pool half the
    /// one below rounding up, and at least two levels for any pool.
    #[test]
    fn levels_halve_the_pool_to_the_depth_of_its_cube() {
        assert_eq!(level_pools(512), [512, 256, 128, 64, 32]);
        assert_eq!(level_pools(8548), [8548, 4274, 2137, 1069, 535, 268, 134]);
        assert_eq!(level_pools(35), [35, 18, 9, 5]);
        assert_eq!(level_pools(7), [7, 4, 2]);
        assert_eq!(level_pools(5), [5, 3, 2]);
        assert_eq!(level_pools(1), [1, 1]);
        // 64³ cells (512 tiles) and 128³ (4096) take today's 5 and 6 levels.
        assert_eq!(level_pools(4096).len(), 6);
        assert_eq!(level_pools(125).len(), 5);
        assert_eq!(level_pools(64).len(), 4);
        assert_eq!(level_pools(8).len(), 3);
        for p in 1..=5000 {
            let pools = level_pools(p);
            assert!(pools.len() >= 2, "{p}");
            assert!(pools.windows(2).all(|w| w[1] == w[0].div_ceil(2)), "{p}");
        }
    }

    /// The solve's prelude is the arm, init, the start's check and the warm
    /// start's residual, and on the engine's stop the warm start's check.
    #[test]
    fn pass_counts_follow_the_levels() {
        let v_cycle = 4 * 11 + 1;
        assert_eq!(passes(512, Stop::Fixed(8)), (27, 4 + 8 * (v_cycle + 6)));
        assert_eq!(passes(512, Stop::Converged(8)).1, 5 + 8 * (v_cycle + 6));
        assert_eq!(passes(1, Stop::Fixed(3)), (9, 4 + 3 * (11 + 1 + 6)));
        assert_eq!(
            round_commands(4, BodyRound::Without),
            (v_cycle as u32 + 6, 0)
        );
        assert_eq!(
            round_commands(4, BodyRound::Gated),
            (v_cycle as u32 + 10, 0)
        );
        assert_eq!(
            round_commands(3, BodyRound::GatedCoarse(1)),
            (3 * 11 + 1 + 11 + 3, 0)
        );
        let slots = Slots { levels: 5 };
        assert_eq!(
            (
                slots.single(),
                slots.dense(0),
                slots.body_partial(),
                slots.body_finalize(),
                slots.triples()
            ),
            (5, 6, 11, 12, 13)
        );
        assert_eq!(
            slots.armed() as usize,
            armed_groups(&level_pools(512)).len()
        );
        // The pooled fine vectors and coarse fields scale with the pool.
        assert!(scratch_bytes(4096) > FINE_BYTES_PER_CELL * pool_len(4096));
        assert!(scratch_bytes(4096) > scratch_bytes(2048));
    }

    /// The gradient never starts on the coarsest level: it is only smoothed.
    #[test]
    fn solve_levels_stop_above_the_coarsest() {
        assert_eq!(max_solve_level(512), 3);
        assert_eq!(max_solve_level(4096), 4);
        assert_eq!(max_solve_level(1), 0);
        assert!(level_refusal(512, 3).is_none());
        assert!(level_refusal(512, 4).unwrap().contains("0 to 3"));
        assert!(level_refusal(1, 1).unwrap().contains("0 to 0"));
    }

    /// A folded reduction writes two partials per slot of its level, and
    /// every level's full group count covers its slots, so a recorded
    /// dispatch covers any list however large the pool is.
    #[test]
    fn reductions_fill_the_pool() {
        for p in [1, 7, 35, 512, 8548] {
            let pools = level_pools(p);
            let groups = armed_groups(&pools);
            for (l, &pool) in pools.iter().enumerate() {
                assert_eq!(groups[l][0], 2 * pool, "{p} level {l}");
                assert_eq!(groups[pools.len() + 1 + l][0], 2 * pool, "{p} dense {l}");
            }
            assert_eq!(partial_count(p), 2 * p);
            // 512 threads cover a slot: two workgroups of THREADS.
            assert_eq!(2 * THREADS, TILE_CELLS);
        }
        assert_eq!(slot_bases(&level_pools(35)), [0, 35, 53, 62, 67]);
    }

    #[test]
    fn pressure_shader_validates_with_every_entry() {
        let source = shader();
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
        let entries: Vec<&str> = module
            .entry_points
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        for entry in [
            "classify_main",
            "lists_main",
            "poison_main",
            "init_main",
            "warm_main",
            "smooth_main",
            "residual_main",
            "restrict_main",
            "prolong_main",
            "update_main",
            "zero_main",
            "parents_main",
            "coarsen_water_main",
            "coarsen_faces_main",
            "rows_main",
            "lentine_flux_main",
        ] {
            assert!(entries.contains(&entry), "missing entry {entry}");
        }
        assert_eq!(size_of::<Params>(), 80, "twenty words, the shader's Params");
    }
}
