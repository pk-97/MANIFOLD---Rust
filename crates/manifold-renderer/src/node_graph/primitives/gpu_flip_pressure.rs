//! GPU FLIP's pressure solver: the multigrid-preconditioned conjugate
//! gradient for L p = f on the water (docs/GPU_FLIP_PRESSURE_SOLVE.md section
//! 3 (the solve)), as a module the step node encodes directly. One solver
//! serves every solve on one water lattice: [`PressureSolver::prepare`] builds
//! the coarse levels and assembles every level's operator rows once (the
//! reference `Level`'s assembly: six face weights and the ghost and plain
//! diagonals per cell), then [`PressureSolver::solve`] runs per right-hand
//! side (the pressure solve and the density solve) reading rows only.
//!
//! The V-cycle depth follows the lattice: each level halves every side,
//! rounding up, until every side is [`COARSEST_SIDE`] or less, and that level
//! is solved exactly by its inverse. An odd side's extra coarse half-cell is
//! solid, so any Resolution runs and the V-cycle stays symmetric
//! (`scripts/mgpcg_reference.py`, the oracle, proves both).
//!
//! A solve may run its gradient on a coarse level instead of the fine one
//! ([`Solve::level`], docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 11
//! (Solve Level)): the right-hand side is restricted down to it by the
//! V-cycle's own transfer, the gradient and the V-cycle below it run on that
//! level's rows and tiles, and the pressure is prolonged back up. The
//! coarsest level is solved by its inverse, not smoothed, so it is never a
//! gradient level.

use manifold_gpu::{GATED_RANGE_BYTES, GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use super::gpu_flip_bodies::{Bodies, BodyPasses};
use crate::node_graph::fluid_particles::FaceSample;

const SHADER: &str = include_str!("shaders/gpu_flip_pressure.wgsl");
const INVERSE_SHADER: &str = include_str!("shaders/coarse_inverse.wgsl");

/// The coarsest level's largest side: at most 4³ = 64 cells, the one
/// workgroup the coarse inverse runs in.
pub(crate) const COARSEST_SIDE: u32 = 4;
/// Cells the coarse inverse takes: its shader's MAX_CELLS.
#[cfg(test)]
const MAX_COARSE_CELLS: u64 = 64;
/// The longest lattice side the solver takes.
pub(crate) const MAX_SIDE: u32 = 1024;
/// Iterations one solve may run: the scalars buffer holds two per iteration.
pub(crate) const MAX_ITERATIONS: u32 = 64;
/// The stop's relative tolerance on |r|∞ / |f|∞, FLIP Fluids'
/// `_pressureSolveTolerance` unchanged: f32 carries the recursive residual
/// below it in 11 to 14 iterations on every saved problem
/// (`pressure_module_converges_on_the_engine_tolerance`).
pub(crate) const TOLERANCE: f32 = 1e-9;
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

/// The V-cycle's lattices, finest first: halve every side, rounding up, while
/// any side is over [`COARSEST_SIDE`].
pub(crate) fn level_lattices(lattice: [u32; 3]) -> Vec<[u32; 3]> {
    let mut levels = vec![lattice];
    let mut n = lattice;
    while n.iter().any(|&side| side > COARSEST_SIDE) {
        n = n.map(|side| side.div_ceil(2));
        levels.push(n);
    }
    levels
}

/// The deepest level a solve's gradient may run on at `lattice`: every
/// level but the coarsest, which is solved exactly and never smoothed.
pub(crate) fn max_solve_level(lattice: [u32; 3]) -> usize {
    level_lattices(lattice).len().saturating_sub(2)
}

/// Why `level` is refused at `lattice`, or None.
pub(crate) fn level_refusal(lattice: [u32; 3], level: usize) -> Option<String> {
    let max = max_solve_level(lattice);
    (level > max).then(|| format!("Solve Level must be 0 to {max} on a {lattice:?} lattice, not {level}"))
}

fn cells(n: [u32; 3]) -> u64 {
    n.iter().map(|&side| u64::from(side)).product()
}

fn face_records(n: [u32; 3]) -> u64 {
    n.iter().map(|&side| u64::from(side) + 1).product()
}

/// The solve's tile: 8³ cells, two workgroups (gpu_flip_pressure.wgsl TILE).
const TILE: u32 = 8;

/// Tiles covering lattice `n`.
fn tile_total(n: [u32; 3]) -> u32 {
    n.iter().map(|&side| side.div_ceil(TILE)).product()
}

/// Each level's first word in the flags and lists, then the total.
fn tile_bases(lattices: &[[u32; 3]]) -> Vec<u32> {
    let mut bases = Vec::with_capacity(lattices.len() + 1);
    let mut base = 0;
    for &n in lattices {
        bases.push(base);
        base += tile_total(n);
    }
    bases.push(base);
    bases
}

/// Device bytes the solver holds for itself at `lattice`: five lattice
/// vectors and the fine rows, each coarse level's water, touched mask,
/// faces, rows, right-hand side and correction, the coarse inverse, every
/// level's tile flags and lists, the partial sums and the scalars.
#[cfg(any(test, feature = "gpu-proofs"))]
pub(crate) fn scratch_bytes(lattice: [u32; 3]) -> u64 {
    let levels = level_lattices(lattice);
    let coarse: u64 = levels[1..].iter().map(|&n| 4 * cells(n) * 4 + cells(n) * ROW_BYTES + face_records(n) * FACE_BYTES).sum();
    let last = cells(*levels.last().expect("at least the fine level"));
    let tiles = u64::from(*tile_bases(&levels).last().expect("the total"));
    5 * cells(lattice) * 4 + cells(lattice) * ROW_BYTES + coarse + last * last * 4 + 2 * tiles * 4 + u64::from(partial_count(lattice)) * 4
        + u64::from(2 * MAX_ITERATIONS) * 4
        + PROGRESS_BYTES
        + 2 * gate_bytes(levels.len())
        + RANGES_BYTES
}

const FACE_BYTES: u64 = size_of::<FaceSample>() as u64;
/// One cell's operator row (gpu_flip_pressure.wgsl Row): six weights and
/// two diagonals.
const ROW_BYTES: u64 = 32;

/// Dispatches one [`PressureSolver::prepare`] and one
/// [`PressureSolver::solve`] of `iterations` encode at `lattice`.
#[cfg(test)]
pub(crate) fn passes(lattice: [u32; 3], iterations: u32) -> (usize, usize) {
    let coarse = level_lattices(lattice).len() - 1;
    let (before, after) = round_commands(coarse, false);
    // Prepare: every level's rows, tile flags and list, each coarse level's
    // water and faces, the inverse. The solve: arming the gate, init and the
    // start's check, then per iteration.
    (5 * coarse + 4, 3 + iterations as usize * (before + after) as usize)
}

/// A conjugate gradient round's gated dispatches with `coarse` levels below
/// the fine one: those up to and including the operator product, then
/// those after it. With `bodies` the body product sits between, ungated,
/// and p · s needs its own pass; without, the operator pass folds it and
/// the round is one piece.
fn round_commands(coarse: usize, bodies: bool) -> (u32, u32) {
    let v_cycle = (coarse * (4 * SMOOTH_ROUNDS + 3) + 1) as u32;
    // v_cycle (r·z folded), finalize, direction, apply | update (|r|∞ folded), check
    if bodies {
        // apply plain | p·s partial, finalize
        (v_cycle + 3, 4)
    } else {
        // apply folds p·s | finalize
        (v_cycle + 6, 0)
    }
}

/// Why a lattice is refused, or None.
pub(crate) fn lattice_refusal(lattice: [u32; 3]) -> Option<String> {
    lattice
        .iter()
        .any(|&side| side == 0 || side > MAX_SIDE)
        .then(|| format!("every lattice side must be 1 to {MAX_SIDE}, not {lattice:?}"))
}

/// The water a solve runs on: the cell lattice, water per cell (> 0.5), the
/// padded face grid of open fractions (the step's open_fractions pass, box
/// walls 0), the cell size in metres, and the particles' signed distance per cell
/// for the free surface's ghost rows (docs/GPU_FLIP_PRESSURE_SOLVE.md
/// section 2 (the equation)). With no φ the rows are the plain ones, as the
/// density solve runs; the coarse levels always are.
pub(crate) struct Water<'a> {
    pub lattice: [u32; 3],
    pub cell_size: f32,
    pub water: &'a GpuBuffer,
    pub faces: &'a GpuBuffer,
    pub phi: Option<&'a GpuBuffer>,
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
    nx: u32,
    ny: u32,
    nz: u32,
    color: u32,
    cx: u32,
    cy: u32,
    cz: u32,
    mode: u32,
    cell_size: f32,
    slot: u32,
    ghost: u32,
    tolerance: f32,
    /// The dispatched level's first word in the tile flags and lists.
    list_base: u32,
    /// The dispatched level's gate triple.
    level: u32,
    /// classify: every tile active (the proofs' oracle).
    all_tiles: u32,
    _pad0: u32,
}

impl Params {
    fn at(n: [u32; 3], cell_size: f32) -> Self {
        Self { nx: n[0], ny: n[1], nz: n[2], cell_size, ..Self::default() }
    }

    fn coarse(mut self, c: [u32; 3]) -> Self {
        [self.cx, self.cy, self.cz] = c;
        self
    }

    /// The level whose tile list the dispatch runs over.
    fn over(mut self, v: &View<'_>) -> Self {
        self.list_base = v.list_base;
        self.level = v.level;
        self
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InverseParams {
    nodes_x: u32,
    nodes_y: u32,
    nodes_z: u32,
    _pad0: u32,
}

struct Pipelines {
    smooth: GpuComputePipeline,
    residual: GpuComputePipeline,
    restrict: GpuComputePipeline,
    prolong: GpuComputePipeline,
    coarsen_water: GpuComputePipeline,
    coarsen_faces: GpuComputePipeline,
    rows: GpuComputePipeline,
    inverse: GpuComputePipeline,
    coarse_solve: GpuComputePipeline,
    init: GpuComputePipeline,
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
        let pipeline = |entry: &str, label: &str| device.create_compute_pipeline(SHADER, entry, label);
        Self {
            classify: pipeline("classify_main", "gpu_flip.pressure.classify"),
            lists: pipeline("lists_main", "gpu_flip.pressure.lists"),
            #[cfg(all(test, feature = "gpu-proofs"))]
            poison: pipeline("poison_main", "gpu_flip.pressure.poison"),
            smooth: pipeline("smooth_main", "gpu_flip.pressure.smooth"),
            residual: pipeline("residual_main", "gpu_flip.pressure.residual"),
            restrict: pipeline("restrict_main", "gpu_flip.pressure.restrict"),
            prolong: pipeline("prolong_main", "gpu_flip.pressure.prolong"),
            coarsen_water: pipeline("coarsen_water_main", "gpu_flip.pressure.coarsen_water"),
            coarsen_faces: pipeline("coarsen_faces_main", "gpu_flip.pressure.coarsen_faces"),
            rows: pipeline("rows_main", "gpu_flip.pressure.rows"),
            inverse: device.create_compute_pipeline(INVERSE_SHADER, "inverse_main", "gpu_flip.pressure.coarse_inverse"),
            coarse_solve: pipeline("coarse_solve_main", "gpu_flip.pressure.coarse_solve"),
            init: pipeline("init_main", "gpu_flip.pressure.init"),
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

/// A coarse level's own arrays.
struct Level {
    lattice: [u32; 3],
    cell_size: f32,
    water: GpuBuffer,
    /// 1 where any fine child is touched: the level's tile classifier reads
    /// it as its water.
    touched: GpuBuffer,
    faces: GpuBuffer,
    rows: GpuBuffer,
    rhs: GpuBuffer,
    e: GpuBuffer,
}

/// Everything sized by the lattice, rebuilt when it changes.
struct Buffers {
    lattice: [u32; 3],
    coarse: Vec<Level>,
    /// The fine level's operator rows, built at prepare.
    rows: GpuBuffer,
    r: GpuBuffer,
    z: GpuBuffer,
    p: GpuBuffer,
    /// A V-cycle level's residual, then the iteration's s = −L p.
    scratch: GpuBuffer,
    /// A coarse-level solve with bodies: the fine body product on the
    /// prolonged direction, before it is restricted into s.
    body: GpuBuffer,
    inverse: GpuBuffer,
    /// Two floats per fine-lattice tile: a folded reduction's partials.
    partials: GpuBuffer,
    scalars: GpuBuffer,
    /// The stop's record (gpu_flip_pressure.wgsl check_main).
    progress: GpuBuffer,
    /// Every level's tile flags (1 active) and active-tile lists, level
    /// after level from `bases`, built at prepare.
    flags: GpuBuffer,
    lists: GpuBuffer,
    bases: Vec<u32>,
    /// Every gated dispatch's group triple, [`Slots`] order: each level's
    /// written by its list builder at prepare, the single one once.
    armed: GpuBuffer,
    /// What a recorded dispatch runs, by triple: every tile, so a replayed
    /// solve covers any list; the kernels return past the live count.
    groups: Vec<[u32; 3]>,
    /// The solve's live copy of `armed`, zeroed when the solve stops.
    gate: GpuBuffer,
    /// The replayed rounds' range entries (`GATED_RANGE_BYTES` each, two a
    /// round): armed with the gate, and zeroed past the round that stops.
    ranges: GpuBuffer,
}

impl Buffers {
    fn new(device: &GpuDevice, lattice: [u32; 3]) -> Self {
        let lattices = level_lattices(lattice);
        let vector = |n: [u32; 3]| device.create_buffer(cells(n) * 4);
        let rows = |n: [u32; 3]| device.create_buffer(cells(n) * ROW_BYTES);
        let coarse = lattices[1..]
            .iter()
            .map(|&n| Level {
                lattice: n,
                cell_size: 0.0,
                water: vector(n),
                touched: vector(n),
                faces: device.create_buffer(face_records(n) * FACE_BYTES),
                rows: rows(n),
                rhs: vector(n),
                e: vector(n),
            })
            .collect();
        let last = cells(*lattices.last().expect("at least the fine level"));
        let bases = tile_bases(&lattices);
        let tiles = u64::from(*bases.last().expect("the total"));
        Self {
            lattice,
            coarse,
            rows: rows(lattice),
            r: vector(lattice),
            z: vector(lattice),
            p: vector(lattice),
            scratch: vector(lattice),
            body: vector(lattice),
            inverse: device.create_buffer(last * last * 4),
            partials: device.create_buffer(u64::from(partial_count(lattice)) * 4),
            scalars: device.create_buffer(u64::from(2 * MAX_ITERATIONS) * 4),
            progress: device.create_buffer(PROGRESS_BYTES),
            flags: device.create_buffer(tiles * 4),
            lists: device.create_buffer(tiles * 4),
            bases,
            armed: armed(device, &lattices),
            groups: armed_groups(&lattices),
            gate: device.create_buffer(gate_bytes(lattices.len())),
            ranges: device.create_buffer(RANGES_BYTES),
        }
    }
}

/// Floats in the stop's record: four, then |r|∞ per iteration.
pub(crate) const PROGRESS_FLOATS: u32 = 4 + MAX_ITERATIONS;
const PROGRESS_BYTES: u64 = PROGRESS_FLOATS as u64 * 4;
/// Bytes of one indirect dispatch's three group counts.
const TRIPLE_BYTES: u64 = 12;
/// Range entries a solve's rounds take: two a round, every round the cap
/// allows (gpu_flip_pressure.wgsl ROUNDS).
const RANGES_BYTES: u64 = 2 * MAX_ITERATIONS as u64 * GATED_RANGE_BYTES;

/// Where each gated dispatch's groups sit in the gate, by triple: level l's
/// lattice at l, then one group.
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
    fn triples(self) -> u32 {
        self.levels as u32 + 1
    }
}

fn gate_bytes(levels: usize) -> u64 {
    u64::from(Slots { levels }.triples()) * TRIPLE_BYTES
}

/// Partials a folded reduction over `n` writes: two per tile of the
/// lattice, active or not, however large it is.
fn partial_count(n: [u32; 3]) -> u32 {
    2 * tile_total(n)
}

/// The gate's full group counts for `lattices`, finest first, by triple:
/// every tile's two workgroups, then one group.
fn armed_groups(lattices: &[[u32; 3]]) -> Vec<[u32; 3]> {
    let mut triples: Vec<[u32; 3]> = lattices.iter().map(|&n| [partial_count(n), 1, 1]).collect();
    triples.push([1, 1, 1]);
    triples
}

/// [`armed_groups`] on the device; each prepare overwrites the level
/// triples with the live tile counts.
fn armed(device: &GpuDevice, lattices: &[[u32; 3]]) -> GpuBuffer {
    let words: Vec<u32> = armed_groups(lattices).into_iter().flatten().collect();
    let buffer = device.create_buffer_shared(words.len() as u64 * 4);
    // SAFETY: the buffer is shared, exactly `words` long, and no GPU work has
    // been encoded against it yet.
    unsafe { buffer.write(0, bytemuck::cast_slice(&words)) };
    buffer
}

fn bytes(params: &Params) -> GpuBinding<'_> {
    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(params) }
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer { binding, buffer, offset: 0 }
}

fn groups(threads: u64) -> [u32; 3] {
    [threads.div_ceil(u64::from(THREADS)) as u32, 1, 1]
}

/// One level's view for the V-cycle: the fine level reads the caller's water
/// and its rows and works in r and z; coarse levels' rows are plain.
#[derive(Clone, Copy)]
struct View<'a> {
    lattice: [u32; 3],
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
    /// The level's first word in the tile flags and lists, and its gate
    /// triple.
    list_base: u32,
    level: u32,
}

/// Level `level`'s view: the fine level is the caller's water with the
/// solver's r and z; a coarse level is its own arrays.
fn view<'a>(b: &'a Buffers, water: &Water<'a>, level: usize) -> View<'a> {
    match level {
        0 => View {
            lattice: water.lattice,
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
        },
        l => {
            let c = &b.coarse[l - 1];
            View {
                lattice: c.lattice,
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

/// One solve's inputs.
pub(crate) struct Solve<'a> {
    pub rhs: &'a GpuBuffer,
    pub pressure: &'a GpuBuffer,
    pub stop: Stop,
    pub bodies: Option<(&'a BodyPasses, &'a Bodies<'a>)>,
    /// The V-cycle level the gradient runs on (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md
    /// section 11 (Solve Level)); 0 is the fine lattice. At most
    /// [`max_solve_level`].
    pub level: usize,
    /// Runs on a coarse level's right-hand side (the buffer, its lattice and
    /// cell size) once the fine one is restricted down to it, before the
    /// gradient starts: the step's pocket mean at that level.
    pub coarse_rhs: Option<CoarseRhs<'a>>,
}

/// A pass over a coarse level's right-hand side: the buffer, its lattice
/// and its cell size.
pub(crate) type CoarseRhs<'a> = &'a dyn Fn(&mut GpuEncoder, &GpuBuffer, [u32; 3], f32);

#[derive(Default)]
pub(crate) struct PressureSolver {
    pipelines: Option<Pipelines>,
    buffers: Option<Buffers>,
    /// The lattice and cell size the levels were last built for, and
    /// whether the fine rows carry the ghost diagonal (prepare saw φ).
    prepared: Option<([u32; 3], f32, bool)>,
}

impl PressureSolver {
    /// Build the solver's pipelines; the owning node calls this at install.
    pub(crate) fn prepare_pipelines(&mut self, device: &GpuDevice) {
        if self.pipelines.is_none() {
            self.pipelines = Some(Pipelines::new(device));
        }
    }

    /// Build every level's operator rows, the coarse levels and the coarse
    /// inverse for `water`. Every solve until the next prepare runs on this
    /// water; a solve with φ needs a prepare that saw it. Allocates only when
    /// the lattice changes.
    pub(crate) fn prepare(&mut self, device: &GpuDevice, enc: &mut GpuEncoder, water: &Water<'_>) -> Result<(), String> {
        if let Some(reason) = lattice_refusal(water.lattice) {
            return Err(reason);
        }
        let n = water.lattice;
        let phi = water.phi.map_or(u64::MAX, |phi| phi.size);
        if cells(n) * 4 > water.water.size.min(phi) || face_records(n) * FACE_BYTES > water.faces.size {
            return Err(format!("a {n:?} lattice is larger than its water, distance or face arrays"));
        }
        if !(water.cell_size.is_finite() && water.cell_size > 0.0) {
            return Err("the cell size must be positive".into());
        }
        let pipes = self.pipelines.as_ref().expect("pressure pipelines built by prepare_pipelines at install");
        if self.buffers.as_ref().is_none_or(|b| b.lattice != n) {
            self.buffers = Some(Buffers::new(device, n));
        }
        let b = self.buffers.as_mut().expect("buffers sized");
        let mut h = water.cell_size;
        for level in &mut b.coarse {
            h *= 2.0;
            level.cell_size = h;
        }
        let (ghost, phi) = water.ghost();
        let assemble = |enc: &mut GpuEncoder, lattice: [u32; 3], water: &GpuBuffer, faces: &GpuBuffer, ghost: (u32, &GpuBuffer), rows: &GpuBuffer| {
            enc.dispatch_compute(
                &pipes.rows,
                &[
                    bytes(&Params { ghost: ghost.0, ..Params::at(lattice, 0.0) }),
                    buffer(1, water),
                    buffer(2, faces),
                    buffer(10, ghost.1),
                    buffer(18, rows),
                ],
                groups(cells(lattice)),
                "gpu_flip.pressure.rows",
            );
        };
        // A level's tile flags and active list from its touched mask: the
        // fine level's water, a coarse level's any-touched-child mask.
        let all_tiles = u32::from(super::gpu_flip_step::all_tiles());
        let tiles = |enc: &mut GpuEncoder, lattice: [u32; 3], touched: &GpuBuffer, level: usize| {
            let params = Params { list_base: b.bases[level], level: level as u32, all_tiles, ..Params::at(lattice, 0.0) };
            enc.dispatch_compute(
                &pipes.classify,
                &[bytes(&params), buffer(1, touched), buffer(19, &b.flags)],
                groups(u64::from(tile_total(lattice))),
                "gpu_flip.pressure.classify",
            );
            enc.dispatch_compute(
                &pipes.lists,
                &[bytes(&params), buffer(14, &b.armed), buffer(19, &b.flags), buffer(20, &b.lists)],
                [1, 1, 1],
                "gpu_flip.pressure.lists",
            );
        };
        assemble(enc, n, water.water, water.faces, (ghost, phi), &b.rows);
        tiles(enc, n, water.water, 0);
        let mut fine = (n, water.water, water.water, water.faces);
        for (index, level) in b.coarse.iter().enumerate() {
            let params = Params::at(fine.0, 0.0).coarse(level.lattice);
            enc.dispatch_compute(
                &pipes.coarsen_water,
                &[bytes(&params), buffer(1, fine.1), buffer(4, fine.2), buffer(5, &level.water), buffer(6, &level.touched)],
                groups(cells(level.lattice)),
                "gpu_flip.pressure.coarsen_water",
            );
            enc.dispatch_compute(
                &pipes.coarsen_faces,
                &[bytes(&params), buffer(2, fine.3), buffer(9, &level.faces)],
                groups(face_records(level.lattice)),
                "gpu_flip.pressure.coarsen_faces",
            );
            assemble(enc, level.lattice, &level.water, &level.faces, (0, &level.water), &level.rows);
            tiles(enc, level.lattice, &level.touched, index + 1);
            fine = (level.lattice, &level.water, &level.touched, &level.faces);
        }
        let (last, last_water, _, last_faces) = fine;
        let params = InverseParams { nodes_x: last[0], nodes_y: last[1], nodes_z: last[2], _pad0: 0 };
        enc.dispatch_compute(
            &pipes.inverse,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                buffer(1, last_water),
                buffer(2, last_faces),
                buffer(3, &b.inverse),
            ],
            [1, 1, 1],
            "gpu_flip.pressure.coarse_inverse",
        );
        self.prepared = Some((n, water.cell_size, water.phi.is_some()));
        Ok(())
    }

    /// Solve L p = `run.rhs` on the prepared water by conjugate gradient
    /// iterations from zero, one V-cycle each; p into `run.pressure`.
    /// `run.stop` says when it ends; the solve's record (iterations run,
    /// stopped or not) is in [`Self::progress`] once the GPU is done.
    /// `water` must be what [`Self::prepare`] last saw. With bodies, the
    /// dynamic bodies sit inside the operator: every iteration's s = L p gains
    /// their ρh·G M⁻¹ Gᵀ p (`scripts/mgpcg_reference.py` `body_solve`). The
    /// V-cycle sees the water alone.
    ///
    /// At `run.level` k > 0 the gradient runs on level k: the right-hand side
    /// goes down by k restrictions (the V-cycle's Rᵀ, the first masked as
    /// init masks), the gradient and the V-cycle below it run on level k's
    /// rows and tiles, and the pressure comes up by k prolongations into the
    /// zeroed fine pressure. The bodies stay fine: each direction is
    /// prolonged, their product taken there and restricted into s, so the
    /// coarse operator is L_k + Rᵀ B P.
    pub(crate) fn solve(&mut self, enc: &mut GpuEncoder, water: &Water<'_>, run: Solve<'_>) -> Result<(), String> {
        let n = water.lattice;
        let Solve { rhs, pressure, stop, bodies, level: k, coarse_rhs } = run;
        let Some((lattice, cell_size, with_phi)) = self.prepared else {
            return Err("the solver was not prepared".into());
        };
        if (lattice, cell_size) != (n, water.cell_size) {
            return Err(format!("the solver was not prepared for a {n:?} lattice at this cell size"));
        }
        if water.phi.is_some() && !with_phi {
            return Err("the solver was prepared without the free surface's distance".into());
        }
        let iterations = stop.cap();
        if !(1..=MAX_ITERATIONS).contains(&iterations) {
            return Err(format!("iterations must be 1 to {MAX_ITERATIONS}, not {iterations}"));
        }
        if cells(n) * 4 > rhs.size.min(pressure.size) {
            return Err(format!("a {n:?} lattice is larger than the right-hand side or the pressure"));
        }
        if let Some(reason) = level_refusal(n, k) {
            return Err(reason);
        }
        let (Some(pipes), Some(b)) = (self.pipelines.as_ref(), self.buffers.as_ref()) else {
            return Err("the solver was not prepared".into());
        };
        let slots = Slots { levels: b.coarse.len() + 1 };
        let g = Gate { buffer: &b.gate, ranges: &b.ranges, groups: &b.groups, slots, gated: true, tiles: [&b.armed, &b.flags, &b.lists] };
        // The gate is armed just below, so the prelude always runs full:
        // plain dispatches, which record with the rest of the step.
        let plain = Gate { gated: false, ..g };
        // The gradient's level: its r and z are the solver's, its x and
        // right-hand side the caller's at level 0, level k's own above.
        let top = View { rhs: &b.r, e: &b.z, ..view(b, water, k) };
        let (rhs_k, x): (&GpuBuffer, &GpuBuffer) = if k == 0 { (rhs, pressure) } else { (spare(b, k, Spare::Rhs), spare(b, k, Spare::E)) };
        let chain = Chain { pipes, b, water, g: plain, k };
        if k > 0 {
            chain.restrict_down(enc, rhs, rhs_k, RESTRICT_MASK, 0);
            if let Some(hook) = coarse_rhs {
                hook(enc, rhs_k, top.lattice, top.cell_size);
            }
        }
        // Each round is one replayed segment, or two around the ungated body
        // product: the arm writes every round's range entries as live and
        // the stop zeroes the rounds after it, with the gate.
        let (before, after) = round_commands(b.coarse.len() - k, bodies.is_some());
        let fine = Params { cx: slots.triples(), tolerance: stop.tolerance(), ..Params::at(top.lattice, top.cell_size).over(&top) };
        let arm = Params { color: before, slot: after, ..fine };
        enc.dispatch_compute(
            &pipes.arm,
            &[bytes(&arm), buffer(12, &b.gate), buffer(14, &b.armed), buffer(15, &b.ranges)],
            [1, 1, 1],
            "gpu_flip.pressure.arm",
        );
        plain.dispatch(
            enc,
            &pipes.init,
            &[
                bytes(&Params { mode: REDUCE, ..fine }),
                buffer(1, top.water),
                buffer(2, top.faces),
                buffer(3, rhs_k),
                buffer(5, &b.r),
                buffer(6, x),
                buffer(16, &b.partials),
            ],
            slots.level(k),
            "gpu_flip.pressure.init",
        );
        check(enc, pipes, b, plain, &Params { mode: 1, ..fine });
        for i in 0..iterations {
            g.begin_round(enc, 2 * i, before);
            v_cycle(enc, pipes, b, water, g, &top);
            dot_finalize(enc, pipes, b, g, &top, 2 * i);
            let step = Params { slot: i, ..fine };
            g.dispatch(
                enc,
                &pipes.direction,
                &[bytes(&step), buffer(3, &b.z), buffer(5, &b.p), buffer(7, &b.scalars)],
                slots.level(k),
                "gpu_flip.pressure.direction",
            );
            // With no bodies s is final here, so the pass folds p · s.
            let apply = Params { mode: 1 | if bodies.is_some() { 0 } else { REDUCE }, ghost: top.ghost, ..fine };
            g.dispatch(
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
                ],
                slots.level(k),
                "gpu_flip.pressure.apply",
            );
            // The body product runs ungated: after a stop it writes s and
            // its sums, which nothing reads again this solve. On a coarse
            // level the direction goes down to the fine lattice (the fine
            // pressure is spare until the end) and the product comes back up.
            if let Some((passes, bodies)) = bodies {
                enc.end_gated_segments();
                if k == 0 {
                    passes.apply(enc, bodies, g.tiles, &b.p, &b.scratch)?;
                } else {
                    chain.prolong_down(enc, &b.p, pressure, Spare::E);
                    zero(enc, pipes, n, &b.body);
                    passes.apply(enc, bodies, g.tiles, pressure, &b.body)?;
                    chain.restrict_down(enc, &b.body, &b.scratch, 0, RESTRICT_ADD);
                }
                g.begin_round(enc, 2 * i + 1, after);
                g.dispatch(
                    enc,
                    &pipes.dot_partial,
                    &[bytes(&fine), buffer(3, &b.p), buffer(4, &b.scratch), buffer(16, &b.partials)],
                    slots.level(k),
                    "gpu_flip.pressure.dot_partial",
                );
            }
            dot_finalize(enc, pipes, b, g, &top, 2 * i + 1);
            g.dispatch(
                enc,
                &pipes.update,
                &[
                    bytes(&Params { mode: REDUCE, ..step }),
                    buffer(3, &b.p),
                    buffer(4, &b.scratch),
                    buffer(5, x),
                    buffer(6, &b.r),
                    buffer(7, &b.scalars),
                    buffer(16, &b.partials),
                ],
                slots.level(k),
                "gpu_flip.pressure.update",
            );
            check(enc, pipes, b, g, &step);
        }
        enc.end_gated_segments();
        if k > 0 {
            chain.prolong_down(enc, x, pressure, Spare::E);
        }
        Ok(())
    }

    /// The fine level's tile buffers after a prepare (the gate triples, the
    /// flags, the lists; the fine level's words start at 0 in each), for
    /// the body passes that run outside the solve.
    pub(crate) fn tiles(&self) -> Result<[&GpuBuffer; 3], String> {
        let b = self.buffers.as_ref().ok_or("the solver was not prepared")?;
        debug_assert_eq!(b.bases[0], 0, "the fine level's flags and lists start at word 0");
        Ok([&b.armed, &b.flags, &b.lists])
    }

    /// The last solve's record: |f|∞, iterations run, 1.0 when it stopped by
    /// the tolerance, then |r|∞ per iteration; [`PROGRESS_FLOATS`] floats.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn progress(&self) -> Option<&GpuBuffer> {
        self.buffers.as_ref().map(|b| &b.progress)
    }

    /// Test-only: NaN into every cell outside each level's active tiles in
    /// the vectors a solve reads inside them (z, p, the residual scratch and
    /// the body product, indexed by the gradient's `level`; each coarse
    /// level's right-hand side and correction) and into the fine level's
    /// inactive partials, after a prepare. A solve that reads past its
    /// lists shows in its pressure.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn poison(&self, enc: &mut GpuEncoder, level: usize) {
        let (Some(pipes), Some(b)) = (self.pipelines.as_ref(), self.buffers.as_ref()) else {
            panic!("the solver was not prepared");
        };
        let top = if level == 0 { b.lattice } else { b.coarse[level - 1].lattice };
        let mut targets = vec![(top, level as u32, &b.z), (top, level as u32, &b.p), (top, level as u32, &b.scratch), (b.lattice, 0, &b.body)];
        for (index, level) in b.coarse.iter().enumerate() {
            targets.push((level.lattice, index as u32 + 1, &level.rhs));
            targets.push((level.lattice, index as u32 + 1, &level.e));
        }
        let partials = level as u32;
        for (lattice, level, out) in targets {
            let params = Params { list_base: b.bases[level as usize], level, color: u32::from(level == partials), ..Params::at(lattice, 0.0) };
            enc.dispatch_compute(
                &pipes.poison,
                &[bytes(&params), buffer(5, out), buffer(16, &b.partials), buffer(19, &b.flags)],
                [partial_count(lattice), 1, 1],
                "gpu_flip.pressure.poison",
            );
        }
    }

    /// Add the last solve, run with `stop`, to the tick's solver words at
    /// `offset` bytes in `into` (gpu_flip_pressure.wgsl tally_main): its
    /// iterations to `word`, and 1 to word 2 when it reached its cap without
    /// meeting the tolerance. `clear` zeroes the words first.
    pub(crate) fn tally(&self, enc: &mut GpuEncoder, stop: Stop, into: &GpuBuffer, offset: u64, word: u32, clear: bool) -> Result<(), String> {
        let (Some(pipes), Some(b)) = (self.pipelines.as_ref(), self.buffers.as_ref()) else {
            return Err("the solver was not prepared".into());
        };
        let params = Params { slot: word, mode: u32::from(clear), tolerance: stop.tolerance(), ..Params::default() };
        enc.dispatch_compute(
            &pipes.tally,
            &[bytes(&params), buffer(11, &b.progress), GpuBinding::Buffer { binding: 13, buffer: into, offset }],
            [1, 1, 1],
            "gpu_flip.pressure.tally",
        );
        Ok(())
    }
}

/// When a solve ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Stop {
    /// Exactly this many iterations.
    Fixed(u32),
    /// The engine's stop (gpu_flip_pressure.wgsl check_main), after at most
    /// this many iterations.
    Converged(u32),
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

#[cfg(all(test, feature = "gpu-proofs"))]
impl PressureSolver {
    /// Copies the last solve's scalars (iteration k's r·z at 2k, p·s at
    /// 2k + 1) into `into`, a shared buffer of 2 · MAX_ITERATIONS floats.
    pub(crate) fn copy_scalars(&self, enc: &mut GpuEncoder, into: &GpuBuffer) {
        let b = self.buffers.as_ref().expect("the solver was prepared");
        enc.copy_buffer_to_buffer(&b.scalars, into, b.scalars.size);
    }

    /// Copies the fine level's rows (`ROW_BYTES` a cell, gpu_flip_pressure.wgsl
    /// Row) into `into`, a shared buffer of the lattice's cells × `ROW_BYTES`.
    pub(crate) fn copy_rows(&self, enc: &mut GpuEncoder, into: &GpuBuffer) {
        let b = self.buffers.as_ref().expect("the solver was prepared");
        enc.copy_buffer_to_buffer(&b.rows, into, b.rows.size);
    }

    /// Coarse level `k`'s lattice and cell size, and copies of its rows, its
    /// right-hand side and its solution after a solve started on it (the
    /// level's own `rhs` and `e`, which that solve's V-cycle never touches)
    /// into shared buffers of the level's cells × `ROW_BYTES`, × 4 and × 4.
    pub(crate) fn copy_level(&self, enc: &mut GpuEncoder, k: usize, rows: &GpuBuffer, rhs: &GpuBuffer, e: &GpuBuffer) -> ([u32; 3], f32) {
        let b = self.buffers.as_ref().expect("the solver was prepared");
        let c = &b.coarse[k - 1];
        enc.copy_buffer_to_buffer(&c.rows, rows, c.rows.size);
        enc.copy_buffer_to_buffer(&c.rhs, rhs, c.rhs.size);
        enc.copy_buffer_to_buffer(&c.e, e, c.e.size);
        (c.lattice, c.cell_size)
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
    ranges: &'a GpuBuffer,
    groups: &'a [[u32; 3]],
    slots: Slots,
    gated: bool,
    /// The tile lists every dispatch reads its cells and partials through:
    /// the live counts, the flags and the lists.
    tiles: [&'a GpuBuffer; 3],
}

/// The most bindings one solve dispatch names, the tile buffers included.
const MAX_BINDINGS: usize = 12;

impl Gate<'_> {
    fn dispatch(self, enc: &mut GpuEncoder, pipeline: &GpuComputePipeline, bindings: &[GpuBinding], triple: usize, label: &str) {
        let groups = self.groups[triple];
        let mut all: arrayvec::ArrayVec<GpuBinding, MAX_BINDINGS> = bindings
            .iter()
            .map(|b| match *b {
                GpuBinding::Buffer { binding, buffer, offset } => GpuBinding::Buffer { binding, buffer, offset },
                GpuBinding::Texture { binding, texture } => GpuBinding::Texture { binding, texture },
                GpuBinding::Sampler { binding, sampler } => GpuBinding::Sampler { binding, sampler },
                GpuBinding::Bytes { binding, data } => GpuBinding::Bytes { binding, data },
            })
            .collect();
        all.push(buffer(14, self.tiles[0]));
        all.push(buffer(19, self.tiles[1]));
        all.push(buffer(20, self.tiles[2]));
        if self.gated {
            enc.dispatch_compute_gated(pipeline, &all, groups, self.buffer, triple as u64 * TRIPLE_BYTES, label);
        } else {
            enc.dispatch_compute(pipeline, &all, groups, label);
        }
    }

    /// Open the gated segment of range entry `index`, `commands` dispatches long.
    fn begin_round(self, enc: &mut GpuEncoder, index: u32, commands: u32) {
        enc.begin_gated_segment(self.ranges, index, commands);
    }
}

/// z = V(r): one V-cycle for L e = r from zero, from the gradient's level
/// `top` down. The last sweep at `top` folds r · z into the partials.
fn v_cycle<'a>(enc: &mut GpuEncoder, pipes: &Pipelines, b: &'a Buffers, water: &Water<'a>, g: Gate<'_>, top: &View<'a>) {
    let start = top.level as usize;
    let at = |level: usize| -> View<'a> { if level == start { *top } else { view(b, water, level) } };
    let last = b.coarse.len();
    for level in start..last {
        let v = at(level);
        let coarse = at(level + 1);
        for round in 0..SMOOTH_ROUNDS {
            for color in [0, 1] {
                let sweep = if round == 0 && color == 0 { Sweep::FromZero } else { Sweep::Plain };
                smooth(enc, pipes, &v, color, sweep, g, level);
            }
        }
        let params = Params::at(v.lattice, v.cell_size).coarse(coarse.lattice);
        g.dispatch(
            enc,
            &pipes.residual,
            &[
                bytes(&Params { ghost: v.ghost, ..params.over(&v) }),
                buffer(1, v.water),
                buffer(3, v.rhs),
                buffer(4, v.e),
                buffer(5, &b.scratch),
                buffer(16, v.partials),
                buffer(17, v.rows),
            ],
            g.slots.level(level),
            "gpu_flip.pressure.residual",
        );
        restrict(enc, pipes, g, &v, &coarse, &b.scratch, coarse.rhs, 0);
    }
    let v = at(last);
    let params = Params::at(v.lattice, v.cell_size);
    g.dispatch(
        enc,
        &pipes.coarse_solve,
        &[bytes(&params), buffer(3, &b.inverse), buffer(4, v.rhs), buffer(5, v.e)],
        g.slots.single(),
        "gpu_flip.pressure.coarse_solve",
    );
    for level in (start..last).rev() {
        let v = at(level);
        let coarse = at(level + 1);
        prolong(enc, pipes, g, &v, &coarse, coarse.e, v.e);
        for round in 0..SMOOTH_ROUNDS {
            for color in [1, 0] {
                let last = level == start && round == SMOOTH_ROUNDS - 1 && color == 0;
                let sweep = if last { Sweep::Fold } else { Sweep::Plain };
                smooth(enc, pipes, &v, color, sweep, g, level);
            }
        }
    }
}

/// `src` on `fine` restricted into `out` on `coarse`, over the coarse
/// level's list; `mode` is the restrict bits.
fn restrict(enc: &mut GpuEncoder, pipes: &Pipelines, g: Gate<'_>, fine: &View<'_>, coarse: &View<'_>, src: &GpuBuffer, out: &GpuBuffer, mode: u32) {
    let params = Params { mode, ..Params::at(fine.lattice, fine.cell_size).coarse(coarse.lattice).over(coarse) };
    g.dispatch(
        enc,
        &pipes.restrict,
        &[bytes(&params), buffer(1, fine.water), buffer(2, fine.faces), buffer(3, src), buffer(5, out), buffer(8, coarse.water)],
        g.slots.level(coarse.level as usize),
        "gpu_flip.pressure.restrict",
    );
}

/// `src` on `coarse` prolonged and added into `out` on `fine`'s water,
/// over the fine level's list.
fn prolong(enc: &mut GpuEncoder, pipes: &Pipelines, g: Gate<'_>, fine: &View<'_>, coarse: &View<'_>, src: &GpuBuffer, out: &GpuBuffer) {
    let params = Params::at(fine.lattice, fine.cell_size).coarse(coarse.lattice).over(fine);
    g.dispatch(
        enc,
        &pipes.prolong,
        &[bytes(&params), buffer(1, fine.water), buffer(3, src), buffer(5, out)],
        g.slots.level(fine.level as usize),
        "gpu_flip.pressure.prolong",
    );
}

/// Every cell of `out` on `lattice` to 0: a dispatch, so it records and
/// labels with the solver's passes.
fn zero(enc: &mut GpuEncoder, pipes: &Pipelines, lattice: [u32; 3], out: &GpuBuffer) {
    enc.dispatch_compute(&pipes.zero, &[bytes(&Params::at(lattice, 0.0)), buffer(5, out)], groups(cells(lattice)), "gpu_flip.pressure.zero");
}

/// A transfer chain between the fine level and the gradient's level k,
/// ungated: the fine right-hand side down, a body direction down and its
/// product up, the pressure up.
#[derive(Clone, Copy)]
struct Chain<'a> {
    pipes: &'a Pipelines,
    b: &'a Buffers,
    water: &'a Water<'a>,
    g: Gate<'a>,
    k: usize,
}

impl<'a> Chain<'a> {
    /// `src` on the fine level restricted k times into `out` on level k,
    /// through the spare right-hand sides between; `first` and `last` are
    /// the restrict bits of the first and last step.
    fn restrict_down(self, enc: &mut GpuEncoder, src: &GpuBuffer, out: &GpuBuffer, first: u32, last: u32) {
        let mut from = src;
        for j in 1..=self.k {
            let to = if j == self.k { out } else { spare(self.b, j, Spare::Rhs) };
            let mode = if j == 1 { first } else { 0 } | if j == self.k { last } else { 0 };
            restrict(enc, self.pipes, self.g, &view(self.b, self.water, j - 1), &view(self.b, self.water, j), from, to, mode);
            from = to;
        }
    }

    /// `src` on level k prolonged k times into `out` on the fine level,
    /// through the spare vectors `which` between; every target is zeroed
    /// first, so each holds its level's water values and 0 elsewhere.
    fn prolong_down(self, enc: &mut GpuEncoder, src: &GpuBuffer, out: &GpuBuffer, which: Spare) {
        let mut from = src;
        for j in (0..self.k).rev() {
            let fine = view(self.b, self.water, j);
            let to = if j == 0 { out } else { spare(self.b, j, which) };
            zero(enc, self.pipes, fine.lattice, to);
            prolong(enc, self.pipes, self.g, &fine, &view(self.b, self.water, j + 1), from, to);
            from = to;
        }
    }
}

/// What a sweep does besides relaxing: the first of a level starts from
/// zero; the solve's last fine one folds rhs · e into the partials.
#[derive(Clone, Copy)]
enum Sweep {
    Plain,
    FromZero,
    Fold,
}

/// One red-black sweep of `color` at a level.
fn smooth(enc: &mut GpuEncoder, pipes: &Pipelines, v: &View<'_>, color: u32, sweep: Sweep, g: Gate<'_>, level: usize) {
    let mode = match sweep {
        Sweep::Plain => 0,
        Sweep::FromZero => 1,
        Sweep::Fold => REDUCE,
    };
    let params = Params { color, mode, ghost: v.ghost, ..Params::at(v.lattice, v.cell_size).over(v) };
    g.dispatch(
        enc,
        &pipes.smooth,
        &[bytes(&params), buffer(1, v.water), buffer(3, v.rhs), buffer(5, v.e), buffer(16, v.partials), buffer(17, v.rows)],
        g.slots.level(level),
        "gpu_flip.pressure.smooth",
    );
}

/// The folded partials of a dot product over the gradient's level summed
/// into scalars[slot], in a fixed order.
fn dot_finalize(enc: &mut GpuEncoder, pipes: &Pipelines, b: &Buffers, g: Gate<'_>, top: &View<'_>, slot: u32) {
    let params = Params { color: partial_count(top.lattice), slot, ..Params::at(top.lattice, 0.0).over(top) };
    g.dispatch(
        enc,
        &pipes.dot_finalize,
        &[bytes(&params), buffer(7, &b.scalars), buffer(16, &b.partials)],
        g.slots.single(),
        "gpu_flip.pressure.dot_finalize",
    );
}

/// The folded partials' max, |r|∞, into the stop's record and the stop
/// test: the start with `mode` 1, else after iteration `slot`.
fn check(enc: &mut GpuEncoder, pipes: &Pipelines, b: &Buffers, g: Gate<'_>, step: &Params) {
    let params = Params { color: partial_count([step.nx, step.ny, step.nz]), ..*step };
    g.dispatch(
        enc,
        &pipes.check,
        &[bytes(&params), buffer(11, &b.progress), buffer(12, &b.gate), buffer(15, &b.ranges), buffer(16, &b.partials)],
        g.slots.single(),
        "gpu_flip.pressure.check",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The liquid conformance suite checks codegen bodies for atomics; these
    /// hand shaders have none, so they are checked here.
    #[test]
    fn pressure_solver_uses_no_atomics() {
        for source in [SHADER, INVERSE_SHADER] {
            let stray = super::super::gpu_flip_step::atomic_sites_outside(source, &[]);
            assert!(stray.is_empty(), "atomics outside the allowlist (I8): {stray:#?}");
        }
    }

    /// The level rule, odd sides included, as the reference's --symmetry run
    /// prints it; the coarsest level always fits the coarse inverse.
    #[test]
    fn levels_halve_rounding_up_to_four() {
        let sides = |n: u32| level_lattices([n; 3]).iter().map(|l| l[0]).collect::<Vec<_>>();
        assert_eq!(sides(24), [24, 12, 6, 3]);
        assert_eq!(sides(25), [25, 13, 7, 4]);
        assert_eq!(sides(37), [37, 19, 10, 5, 3]);
        assert_eq!(sides(40), [40, 20, 10, 5, 3]);
        assert_eq!(sides(64), [64, 32, 16, 8, 4]);
        assert_eq!(sides(100), [100, 50, 25, 13, 7, 4]);
        assert_eq!(sides(128), [128, 64, 32, 16, 8, 4]);
        assert_eq!(sides(3), [3]);
        assert_eq!(level_lattices([64, 9, 1]), [[64, 9, 1], [32, 5, 1], [16, 3, 1], [8, 2, 1], [4, 1, 1]]);
        for n in 1..=MAX_SIDE {
            let last = *level_lattices([n, n.div_ceil(3), 1]).last().unwrap();
            assert!(last.iter().all(|&side| side <= COARSEST_SIDE), "{n}");
            assert!(cells(last) <= MAX_COARSE_CELLS);
        }
    }

    #[test]
    fn refusals_name_the_lattice_and_count() {
        assert!(lattice_refusal([64; 3]).is_none());
        assert!(lattice_refusal([0, 64, 64]).unwrap().contains("1 to 1024"));
        assert!(lattice_refusal([2048, 64, 64]).is_some());
    }

    /// Every shader index stays inside the arrays the solver sizes, walked
    /// per pass on the CPU at odd and even lattices: a transfer's fine reads
    /// and a coarsening's children, both through the round-up rule.
    #[test]
    fn transfers_stay_inside_their_lattices() {
        for n in [1, 2, 3, 4, 5, 7, 9, 24, 25, 37, 40] {
            let levels = level_lattices([n, n + 1, n + 2]);
            for pair in levels.windows(2) {
                let (fine, coarse) = (pair[0], pair[1]);
                for a in 0..3 {
                    for c in 0..coarse[a] as i64 {
                        // Restriction gathers fine 2c − 1 .. 2c + 2 inside the
                        // fine lattice; prolongation's parent f / 2 of every
                        // fine cell is a coarse cell.
                        assert!(2 * c < i64::from(fine[a]), "{fine:?} → {coarse:?}");
                    }
                    for f in 0..fine[a] {
                        assert!(f / 2 < coarse[a]);
                    }
                    // A coarse face 1 .. c − 1 reads fine face 2c, inside the
                    // fine face grid's n + 1.
                    assert!(2 * (coarse[a] - 1) <= fine[a]);
                }
            }
        }
    }

    #[test]
    fn pass_counts_follow_the_levels() {
        assert_eq!(passes([64; 3], 8), (24, 3 + 8 * (4 * 11 + 1 + 6)));
        assert_eq!(passes([3; 3], 3), (4, 3 + 3 * 7));
        assert_eq!(round_commands(4, false), (4 * 11 + 1 + 6, 0));
        assert_eq!(round_commands(4, true), (4 * 11 + 1 + 3, 4));
        assert!(scratch_bytes([128; 3]) > 4 * 128 * 128 * 128 * 4);
    }

    /// The gradient never starts on the coarsest level: it is solved by its
    /// inverse, which folds nothing.
    #[test]
    fn solve_levels_stop_above_the_coarsest() {
        assert_eq!(max_solve_level([64; 3]), 3);
        assert_eq!(max_solve_level([128; 3]), 4);
        assert_eq!(max_solve_level([25; 3]), 2);
        assert_eq!(max_solve_level([3; 3]), 0);
        assert!(level_refusal([64; 3], 3).is_none());
        assert!(level_refusal([64; 3], 4).unwrap().contains("0 to 3"));
        assert!(level_refusal([3; 3], 1).unwrap().contains("0 to 0"));
    }

    /// A folded reduction writes two partials per tile of the lattice, with
    /// no cap: the partials buffer and every level's full group count come
    /// from the same count, so a recorded dispatch covers any list however
    /// large the lattice is (BUG-l2h3.22).
    #[test]
    fn reductions_fill_the_lattice() {
        assert_eq!(partial_count([64; 3]), 1024);
        assert_eq!(partial_count([128; 3]), 8192);
        assert_eq!(partial_count([1, 1, 1]), 2);
        assert_eq!(partial_count([9, 8, 7]), 4);
        for n in [[3; 3], [37, 19, 5], [100; 3], [256; 3], [MAX_SIDE, 64, 1]] {
            let independent = 2 * n.iter().map(|&side| side.div_ceil(TILE)).product::<u32>();
            assert_eq!(partial_count(n), independent, "{n:?}");
            assert_eq!(armed_groups(&level_lattices(n))[0][0], independent, "{n:?}");
            // 512 threads cover a tile: two workgroups of THREADS.
            assert_eq!(2 * THREADS, TILE * TILE * TILE);
        }
        assert_eq!(tile_bases(&level_lattices([64; 3])), [0, 512, 576, 584, 585, 586]);
    }

    /// The shader zeroes range entries up to its own round count, which must
    /// be the cap the solver sizes the entries for.
    #[test]
    fn shader_rounds_match_the_iteration_cap() {
        assert!(SHADER.contains(&format!("const ROUNDS: u32 = {MAX_ITERATIONS}u;")));
    }

    #[test]
    fn pressure_shader_validates_with_every_entry() {
        let module = naga::front::wgsl::parse_str(SHADER).unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let entries: Vec<&str> = module.entry_points.iter().map(|e| e.name.as_str()).collect();
        for entry in ["classify_main", "lists_main", "poison_main", "init_main", "smooth_main", "restrict_main", "prolong_main", "update_main", "zero_main"] {
            assert!(entries.contains(&entry), "missing entry {entry}");
        }
        assert_eq!(size_of::<Params>(), 64, "sixteen words, the shader's Params");
    }
}
