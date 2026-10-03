//! `node.gpu_flip_step` — one GPU FLIP water step (docs/GPU_FLIP_PRESSURE_SOLVE.md
//! section 1 (the step)): sort, particle distance, particles to faces,
//! extend, forces, solids, water mask from φ, divergence, pressure solve,
//! projection, extend, constraint, density projection, then the particles
//! move, remove crowded/extreme markers, compact, and emit inflow. One node
//! because no pass has a consumer outside the step and the solve between them
//! is a barriered reduction; the hand kernels live in
//! `shaders/gpu_flip_step.wgsl`, the solver in [`super::gpu_flip_pressure`].
//!
//! Every scratch array is sized from the lattice and the particle slots
//! before any pass is encoded, so no kernel reads `arrayLength`. The face
//! grid output is this node's own storage, exactly one record per padded
//! cell, reallocated when the lattice changes.
//!
//! Extrapolation order/count, marker removal, inflow emission/placement,
//! inflow constrained velocity and outflow removal port
//! FLIP Fluids `fluidsimulation.cpp` (MIT, Copyright (C) 2026 Ryan L. Guy &
//! Dennis Fassbaender; see THIRD_PARTY_NOTICES.md), line refs in the shader.
//!
//! The narrow-band transport, distance, and reseeding stages port Ferstl et
//! al., "Narrow Band FLIP for Liquid Simulations", Computer Graphics Forum
//! 35(2), 225–232 (2016), doi:10.1111/cgf.12825. FLIP Fluids is credited
//! above for the engine step policies reused by this stage.
//!
//! The density projection is T. Kugelstadt, A. Longva, N. Thuerey and
//! J. Bender, "Implicit Density Projection for Volume Conserving Liquids",
//! IEEE TVCG 27(4), 2019: each step solves a second Poisson equation whose
//! source is the particles' density error against rest, and moves the
//! particles down its gradient. The move is position only and never enters
//! velocity, so it cannot add speed; it restores the volume the divergence
//! solve alone lets drift. Kernel, solid-neighbour weight, surface clamp and
//! ±½ source clamp follow the paper as built in the MIT-licensed `blub`
//! (Copyright (c) 2020 Andreas Reich, github.com/Wumpf/blub,
//! `density_projection_gather_error.comp`; see THIRD_PARTY_NOTICES.md).

use crate::node_graph::primitives::prefix_scan::PrefixScan;
use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use super::gpu_flip_bodies::{Bodies, BodyPasses, REACTION_FLOATS, body_refusal};
use super::gpu_flip_clock::{
    GpuFlipClock, GpuFlipClockInputs, GpuFlipClockParams, flags as clock_flags,
};
use super::gpu_flip_narrow_band::{BandPipelines, NarrowBand, NbParams};
use super::gpu_flip_pressure::{MAX_ITERATIONS, PressureSolver, Solve, Stop, Water, lattice_refusal, level_lattices, level_refusal};
use super::liquid_solid_distance::{SolidDistanceJob, encode_solid_distance};
use super::liquid_stats::{SOLVER_WORDS, with_stats_layout};
use super::prefix_scan::ScanLabels;
use super::sort_particles_into_cells::{LIQUID_PARTICLE_READ, ParticleSorter, SortJob, SortLabels, float_param, int_param};
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid::TICK;
use crate::node_graph::fluid_particles::{FaceSample, FluidParticle};
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::liquid::{EXACT_F32_COUNT, WATER_DENSITY};
use crate::node_graph::liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};
use crate::node_graph::liquid::fields::{FieldBinding, LIQUID_FIELD};
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const STEP_SHADER: &str = include_str!("shaders/gpu_flip_step.wgsl");
const MASK_SHADER: &str = include_str!("shaders/gpu_flip_commit_mask.wgsl");
const NAME: &str = "GPU FLIP Step";

/// Layers of valid faces the step's face grid holds around the water at
/// least: both extensions run `band_layers` ≥ 5.
pub(crate) const FACE_VALID_LAYERS: u32 = 2;
/// The iteration cap when `iterations` is Auto (0): Auto stops on the
/// engine's tolerance (gpu_flip_pressure.rs Stop::Converged).
pub(crate) const AUTO_PRESSURE_ITERATIONS: u32 = MAX_ITERATIONS;
/// The speed the CFL guard is sized for, m/s.
pub(crate) const DEFAULT_TOP_SPEED: f32 = 20.0;
/// Configured engine CFL, shared with the clock.
pub(crate) const ENGINE_CFL: u32 = 5;

/// The CFL guard: the farthest one RK3 stage moves a particle, in cells,
/// `top_speed` over one step rounded up. The inputs are f32, so a ratio
/// within 1e-4 of a whole cell is that cell, not the next.
pub(crate) fn travel_cells(top_speed: f32, step_dt: f32, cell_size: f32) -> u32 {
    (f64::from(top_speed) * f64::from(step_dt) / f64::from(cell_size) - 1e-4).ceil().max(1.0) as u32
}

/// FLIP Fluids _extrapolateFluidVelocities: configured CFL, never travel.
pub(crate) fn band_layers(cfl: u32) -> u32 {
    (3f64.sqrt() * f64::from(cfl)).ceil() as u32 + 3
}

/// Bytes of the step's face grid at `cells`: one record per padded cell.
pub(crate) fn face_bytes(cells: [u32; 3]) -> u64 {
    cells.iter().map(|&n| u64::from(n) + 1).product::<u64>() * size_of::<FaceSample>() as u64
}

/// Half-cell sites sources emit at: eight a cell (gpu_flip_step.wgsl
/// emit_sites).
pub(crate) fn emit_sites(cells: [u32; 3]) -> u64 {
    cells.iter().map(|&n| 2 * u64::from(n)).product()
}

fn cell_bytes(cells: [u32; 3]) -> u64 {
    cells.iter().map(|&n| u64::from(n)).product::<u64>() * 4
}

/// Cells per tile side (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3 (The tile
/// table)). Partial edge tiles are allowed: no lattice side rule.
pub(crate) const TILE: u32 = 8;

/// The cell passes' reach from a particle-holding cell, in cells: the tile
/// set C is every tile within it (`tiles_classify` writes the distance).
/// The shader holds its own copy; the proofs' CPU model reads this one.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) const CELL_REACH: u32 = 2;

/// Tiles per axis, the last one partial when the side is not a multiple of
/// [`TILE`].
pub(crate) fn tile_counts(cells: [u32; 3]) -> [u32; 3] {
    cells.map(|n| n.div_ceil(TILE))
}

pub(crate) fn tile_total(cells: [u32; 3]) -> u64 {
    tile_counts(cells).iter().map(|&t| u64::from(t)).product()
}

/// The farthest tile ring any extend layer reaches at `band` layers: a
/// layer fills faces `1 + layer` cells from the water. A per-step constant,
/// not a cap: the table is sized for it.
pub(crate) fn ring_max(band: u32) -> u32 {
    (1 + band).div_ceil(TILE)
}

/// Words of the tile counts: word 0 the cell set C's size, word k in
/// 1..=ring_max + 1 the tiles with ring ≤ k, then the retired count, then
/// the ring halves' parity.
fn tile_count_words(ring_max: u32) -> u64 {
    u64::from(ring_max) + 4
}

/// Words of the indirect triples: one per count word 0..=ring_max (C, then
/// each ring cap), one for the retired list.
fn tile_args_words(ring_max: u32) -> u64 {
    3 * (u64::from(ring_max) + 2)
}

/// Bytes of the tile table at `cells` for `ring_max`: the nearness, two
/// ring halves, the list by ring, the retired list, the counts and the
/// triples.
#[cfg(any(test, feature = "gpu-proofs"))]
pub(crate) fn tile_scratch_bytes(cells: [u32; 3], ring_max: u32) -> u64 {
    (5 * tile_total(cells) + tile_count_words(ring_max) + tile_args_words(ring_max)) * 4
}

/// Bytes the step holds for itself at `cells` with `slots` particle slots,
/// besides the sort's ranges and the solver's scratch: the sorted particles,
/// [`LATTICE_CELL_ARRAYS`] cell arrays, the solid corners, six face grids, the pocket gate,
/// the pocket sums, the coarse pockets and the tile table.
#[cfg(any(test, feature = "gpu-proofs"))]
pub(crate) fn scratch_bytes(cells: [u32; 3], slots: u64, ring_max: u32) -> u64 {
    let corners = cells.iter().map(|&n| u64::from(n) + 1).product::<u64>() * 4;
    slots.max(1) * size_of::<FluidParticle>() as u64
        + LATTICE_CELL_ARRAYS * cell_bytes(cells)
        + corners
        + 6 * face_bytes(cells)
        + POCKET_GATE_WORDS * 4
        + pocket_sum_bytes(cells)
        + pocket_coarse_bytes(cells)
        + tile_scratch_bytes(cells, ring_max)
        + mask_saved_bytes(cells, slots.max(1))
}

#[cfg(any(test, feature = "gpu-proofs"))]
fn mask_saved_bytes(cells: [u32; 3], slots: u64) -> u64 {
    slots * size_of::<FluidParticle>() as u64
        + face_bytes(cells)
        + cell_bytes(cells)
        + ZERO_BYTES
        + (slots * 8 + u64::from(SOLVER_WORDS) * 4)
}

/// The coarse pockets' bytes: a state and a label per level-1 cell (every
/// deeper level has fewer) and a leader slot per fine cell. A lattice with
/// no level 1 has no solve level above 0 and never reads them.
fn pocket_coarse_bytes(cells: [u32; 3]) -> u64 {
    level_lattices(cells).get(1).map_or(0, |&coarse| 2 * cell_bytes(coarse)) + cell_bytes(cells)
}

/// Test-only lever, approved 2026-10-02 lead; un-suppressed when the executor
/// exposes node access. With it set, `tiles_classify` marks every tile
/// occupied and `tiles_rings` writes ring 0 everywhere, so the sparse passes
/// run dense through the same kernels and lists (design D-6).
#[cfg(all(test, feature = "gpu-proofs"))]
static ALL_TILES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) fn set_all_tiles(on: bool) {
    ALL_TILES.store(on, std::sync::atomic::Ordering::SeqCst);
}

pub(super) fn all_tiles() -> bool {
    #[cfg(all(test, feature = "gpu-proofs"))]
    {
        ALL_TILES.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(all(test, feature = "gpu-proofs")))]
    {
        false
    }
}

/// Test-only lever, approved 2026-10-02 lead; un-suppressed when the executor
/// exposes node access. With it set, the step runs the poison entry
/// (gpu_flip_tile_tests.rs) after the retire: NaN into every cell array of
/// the tiles outside rings 0 and 1 (design section 4 (The defined-value
/// rule)).
#[cfg(all(test, feature = "gpu-proofs"))]
static POISON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) fn set_poison(on: bool) {
    POISON.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// Test-only lever, approved 2026-10-03 lead; un-suppressed when the executor
/// exposes node access. With it set, no solid lets water go: the step skips
/// the separate passes and the second prepare, and runs the solves as before
/// separating solids (GPU_FLIP_PRESSURE_SOLVE.md section 8 (Separating
/// solids)), the bitwise oracle for scenes whose let-go set stays empty.
#[cfg(all(test, feature = "gpu-proofs"))]
static SEPARATE_OFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) fn set_separate_off(on: bool) {
    SEPARATE_OFF.store(on, std::sync::atomic::Ordering::SeqCst);
}

fn separating() -> bool {
    #[cfg(all(test, feature = "gpu-proofs"))]
    {
        !SEPARATE_OFF.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(all(test, feature = "gpu-proofs")))]
    {
        true
    }
}

/// The lattice's cell-sized arrays (`LatticeBuffers`): water, φ, the
/// right-hand side, the pressure, the pocket state and label, the solve
/// mask, the contact mask and the let-go set.
#[cfg(any(test, feature = "gpu-proofs"))]
const LATTICE_CELL_ARRAYS: u64 = 9;

/// Three words a cell (a pocket's 64-bit sum and its count, indexed by its
/// leader cell), then the removed total's two.
fn pocket_sum_bytes(cells: [u32; 3]) -> u64 {
    3 * cell_bytes(cells) + 8
}

/// The shader's `Params`; field meanings are documented there.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct StepParams {
    pub(crate) n: [u32; 3],
    pub(crate) capacity: u32,
    pub(crate) box_min: [f32; 3],
    pub(crate) cell_size: f32,
    pub(crate) gravity: [f32; 3],
    pub(crate) step_dt: f32,
    pub(crate) field_nodes: [u32; 3],
    pub(crate) field_spacing: f32,
    pub(crate) tick_index: i32,
    pub(crate) step_in_tick: i32,
    pub(crate) force_lattices: i32,
    pub(crate) impulse_tick: i32,
    pub(crate) first_tick: i32,
    pub(crate) body_count: i32,
    pub(crate) rows: i32,
    pub(crate) tick_seconds: f32,
    pub(crate) flip: f32,
    pub(crate) max_travel: f32,
    pub(crate) box_offset: f32,
    pub(crate) ghost: u32,
    pub(crate) particles: u32,
    pub(crate) shapes_len: u32,
    pub(crate) rate: f32,
    /// The tank's closed faces, bit 2d the low face of axis d and bit 2d + 1
    /// the high one.
    pub(crate) closed_faces: u32,
    pub(crate) region_count: i32,
    pub(crate) region_rows: i32,
    /// Half-width of an emitted particle's jitter in cells.
    pub(crate) emit_jitter: f32,
    /// The V-cycle level the pressure solves run on (`Step::level`).
    pub(crate) solve_level: u32,
    /// 1: every tile is active (the test-only oracle, [`set_all_tiles`]).
    pub(crate) all_tiles: u32,
    /// The ring a sparse pass's reads are capped at. Unused while the extend
    /// is dense (design D-4); Phase 2's coarse levels take it.
    pub(crate) ring_cap: u32,
    /// [`ring_max`] for this step: the table's extent.
    pub(crate) ring_max: u32,
    /// Ferstl 2016: 0 dense, 1 full-history initialization, 2 masked band.
    pub(crate) narrow_band: u32,
    pub(crate) live_impulse_stride: u32,
    pub(crate) clock_pad: [u32; 3],
}

/// One pass of the step's shader on its own, for the value proofs against
/// the CPU references: `entry` over `threads` threads, `buffers` at their
/// bindings, waited on.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) fn dispatch_pass(device: &GpuDevice, entry: &str, params: &StepParams, buffers: &[(u32, &GpuBuffer)], threads: u64) {
    let pipeline = device.create_compute_pipeline(&step_source(), entry, "node.gpu_flip_step");
    let mut bindings = vec![uniform(params)];
    bindings.extend(buffers.iter().map(|&(binding, b)| buffer(binding, b)));
    let clock_plan = device.create_buffer_shared(48);
    clock_plan.zero_fill();
    if !buffers.iter().any(|&(binding, _)| binding == 46) {
        bindings.push(buffer(46, &clock_plan));
    }
    let mut enc = device.create_encoder("gpu_flip.step.pass");
    enc.dispatch_compute(&pipeline, &bindings, groups(threads), "gpu_flip.step.pass");
    enc.commit_and_wait_completed();
}

struct Pipelines {
    gather: GpuComputePipeline,
    extend: GpuComputePipeline,
    gravity: GpuComputePipeline,
    open: GpuComputePipeline,
    solid_velocity: GpuComputePipeline,
    solid_extrapolate: GpuComputePipeline,
    phi_into_solids: GpuComputePipeline,
    water_from_phi: GpuComputePipeline,
    divergence: GpuComputePipeline,
    distance: GpuComputePipeline,
    subtract: GpuComputePipeline,
    constrain: GpuComputePipeline,
    density: GpuComputePipeline,
    advect: GpuComputePipeline,
    band_distance: GpuComputePipeline,
    band_gather: GpuComputePipeline,
    band_density: GpuComputePipeline,
    narrow_move: GpuComputePipeline,
    narrow_tally: GpuComputePipeline,
    narrow_latch: GpuComputePipeline,
    narrow_disabled: GpuComputePipeline,
    pocket_seed: GpuComputePipeline,
    pocket_start: GpuComputePipeline,
    pocket_round: GpuComputePipeline,
    pocket_sweep: [GpuComputePipeline; 3],
    pocket_check: GpuComputePipeline,
    pocket_condition: GpuComputePipeline,
    pocket_tally: GpuComputePipeline,
    pocket_clear: GpuComputePipeline,
    pocket_accumulate: GpuComputePipeline,
    pocket_remove: GpuComputePipeline,
    pocket_pin: GpuComputePipeline,
    separate_pin: GpuComputePipeline,
    separate_update: GpuComputePipeline,
    /// The removed flux into the pressure, then the density, solver word.
    pocket_flux: [GpuComputePipeline; 2],
    /// The pockets at the solve level: leaders cleared, cells coarsened,
    /// labels moved to the level.
    pocket_coarse: [GpuComputePipeline; 3],
    remove_crowded: GpuComputePipeline,
    emit_flags: GpuComputePipeline,
    emit_write: GpuComputePipeline,
    tiles_classify: GpuComputePipeline,
    tiles_rings: GpuComputePipeline,
    tiles_lists: GpuComputePipeline,
    tiles_fill: GpuComputePipeline,
    tiles_retire: GpuComputePipeline,
    /// The proofs' poison entry, dispatched under [`POISON`].
    #[cfg(all(test, feature = "gpu-proofs"))]
    poison: GpuComputePipeline,
}

fn step_source() -> String {
    let source = STEP_SHADER.replace("return narrow_mask[index] != 0u;", "return true;");
    with_stats_layout(&format!("{LIQUID_POSE}\n{LIQUID_COLLIDER}\n{LIQUID_FIELD}\n{source}"))
}

fn band_source() -> String {
    with_stats_layout(&format!("{LIQUID_POSE}\n{LIQUID_COLLIDER}\n{LIQUID_FIELD}\n{STEP_SHADER}"))
}

impl Pipelines {
    fn new(device: &GpuDevice) -> Self {
        let source = step_source();
        let band = band_source();
        let pipe = |entry: &str| device.create_compute_pipeline(&source, entry, "node.gpu_flip_step");
        let band_pipe = |entry: &str| device.create_compute_pipeline(&band, entry, "node.gpu_flip_step.narrow_band");
        Self {
            gather: pipe("particles_to_faces"),
            extend: pipe("extend_faces"),
            gravity: pipe("face_gravity"),
            open: pipe("open_fractions"),
            solid_velocity: pipe("solid_face_velocity"),
            solid_extrapolate: pipe("solid_extrapolate"),
            phi_into_solids: pipe("phi_into_solids"),
            water_from_phi: pipe("water_from_phi"),
            divergence: pipe("divergence"),
            distance: pipe("particle_distance"),
            subtract: pipe("subtract_pressure"),
            constrain: pipe("constrain_solid_faces"),
            density: pipe("density_source"),
            advect: pipe("faces_to_particles"),
            band_distance: band_pipe("particle_distance"),
            band_gather: band_pipe("particles_to_faces"),
            band_density: band_pipe("density_source"),
            narrow_move: band_pipe("narrow_move"),
            narrow_tally: band_pipe("narrow_tally"),
            narrow_latch: band_pipe("narrow_latch"),
            narrow_disabled: band_pipe("narrow_disabled"),
            pocket_seed: pipe("pocket_seed"),
            pocket_start: pipe("pocket_start"),
            pocket_round: pipe("pocket_round"),
            pocket_sweep: [pipe("pocket_sweep_x"), pipe("pocket_sweep_y"), pipe("pocket_sweep_z")],
            pocket_check: pipe("pocket_check"),
            pocket_condition: pipe("pocket_condition"),
            pocket_tally: pipe("pocket_tally"),
            pocket_clear: pipe("pocket_clear"),
            pocket_accumulate: pipe("pocket_accumulate"),
            pocket_remove: pipe("pocket_remove"),
            pocket_pin: pipe("pocket_pin"),
            separate_pin: pipe("separate_pin"),
            separate_update: pipe("separate_update"),
            pocket_flux: [pipe("pocket_flux_pressure"), pipe("pocket_flux_density")],
            pocket_coarse: [pipe("pocket_leader_clear"), pipe("pocket_coarsen"), pipe("pocket_relabel")],
            remove_crowded: pipe("remove_crowded_markers"),
            emit_flags: pipe("emit_flags"),
            emit_write: pipe("emit_write"),
            tiles_classify: pipe("tiles_classify"),
            tiles_rings: pipe("tiles_rings"),
            tiles_lists: pipe("tiles_lists"),
            tiles_fill: pipe("tiles_fill"),
            tiles_retire: pipe("tiles_retire"),
            #[cfg(all(test, feature = "gpu-proofs"))]
            poison: pipe(super::gpu_flip_tile_tests::POISON_ENTRY),
        }
    }
}

/// The tile table (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3 (The tile
/// table)), built on the GPU every step from the sort's ranges and never
/// read back. Sized for one `ring_max`.
struct TileTable {
    ring_max: u32,
    /// Per tile, the Chebyshev cell distance from its box to the nearest
    /// particle-holding cell: 0..=CELL_REACH, or CELL_REACH + 1 beyond.
    near: GpuBuffer,
    /// Two halves of one tile each, the tile's list rank (0 in C); the
    /// parity word in `counts` names the current half. Zeroed once: the
    /// first step's "previous" ranks are all 0, so every tile outside C is
    /// retired then.
    rank: GpuBuffer,
    /// Every tile: C first, then the rest of rings 0 and 1, then ring by ring.
    by_ring: GpuBuffer,
    /// [`tile_count_words`]; zeroed once, for the parity word.
    counts: GpuBuffer,
    /// [`tile_args_words`] indirect triples.
    args: GpuBuffer,
    /// Tiles in C on the previous step and not on this one.
    retired: GpuBuffer,
}

/// Byte offset of the retired list's indirect triple in `args`.
fn retired_args_offset(ring_max: u32) -> u64 {
    12 * (u64::from(ring_max) + 1)
}

/// `bound` plus the arrays the fill, the retire and the poison write: the
/// gathered faces and the C passes' cell arrays.
fn canonical<'a>(l: &'a LatticeBuffers, mut bound: Vec<GpuBinding<'a>>) -> Vec<GpuBinding<'a>> {
    bound.extend([buffer(4, &l.g), buffer(33, &l.water), buffer(34, &l.phi), buffer(35, &l.rhs)]);
    bound
}

impl TileTable {
    fn new(device: &GpuDevice, cells: [u32; 3], ring_max: u32) -> Result<Self, String> {
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
/// at the solver words).
fn encode_tiles(enc: &mut GpuEncoder, pipes: &Pipelines, params: &StepParams, t: &TileTable, ranges: &GpuBuffer, capped: &GpuBuffer, tally: u64) {
    let tiles = groups(tile_total(params.n));
    enc.dispatch_compute(
        &pipes.tiles_classify,
        &[uniform(params), buffer(1, ranges), buffer(27, &t.near), buffer(30, &t.counts)],
        tiles,
        "gpu_flip.step.tiles.classify",
    );
    enc.dispatch_compute(
        &pipes.tiles_rings,
        &[uniform(params), buffer(27, &t.near), buffer(28, &t.rank), buffer(30, &t.counts)],
        tiles,
        "gpu_flip.step.tiles.rings",
    );
    enc.dispatch_compute(
        &pipes.tiles_lists,
        &[
            uniform(params),
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

/// Scratch for one lattice.
struct LatticeBuffers {
    cells: [u32; 3],
    water: GpuBuffer,
    phi: GpuBuffer,
    rhs: GpuBuffer,
    pressure: GpuBuffer,
    corners: GpuBuffer,
    /// The particles' faces as gathered over C; canonical elsewhere.
    g: GpuBuffer,
    /// The saved (old) faces: `g` extended.
    a: GpuBuffer,
    /// Extension scratch.
    b: GpuBuffer,
    /// The forced, projected, constrained faces.
    f: GpuBuffer,
    /// Open fractions.
    s: GpuBuffer,
    /// The solids' face velocity and friction.
    v: GpuBuffer,
    /// Each cell's sealed-pocket state.
    pocket: GpuBuffer,
    /// The pocket spread's indirect sizes and flags.
    pocket_gate: GpuBuffer,
    /// Each water cell's pocket label.
    pocket_label: GpuBuffer,
    /// [`pocket_sum_bytes`].
    pocket_sum: GpuBuffer,
    /// The solves' water: `water` less each sealed pocket's leader cell.
    solve_water: GpuBuffer,
    /// The main solve's water: `solve_water` less each let-go cell.
    contact_water: GpuBuffer,
    /// 1 where water touching a solid is let go; carried step to step.
    let_go: GpuBuffer,
    /// The pockets at the solve level: state and label per level cell,
    /// sized for level 1 ([`pocket_coarse_bytes`]).
    pocket_coarse: GpuBuffer,
    pocket_coarse_label: GpuBuffer,
    /// By fine label, the lowest level cell of that pocket.
    pocket_leader: GpuBuffer,
}

/// Words of the pocket spread's gate (gpu_flip_step.wgsl `pocket_gate`).
const POCKET_GATE_WORDS: u64 = 11;

fn allocate(device: &GpuDevice, bytes: u64) -> Result<GpuBuffer, String> {
    crate::node_graph::scene_modifier_expand::admit_candidate_bytes(device.modifier_memory_snapshot(), bytes)
        .map_err(|error| error.to_string())
        .and_then(|()| device.try_create_buffer(bytes))
}

/// A buffer that carries state from step to step: shared, so it starts at
/// zero without an encoder.
fn allocate_zeroed(device: &GpuDevice, bytes: u64) -> Result<GpuBuffer, String> {
    let buffer = crate::node_graph::scene_modifier_expand::admit_candidate_bytes(device.modifier_memory_snapshot(), bytes)
        .map_err(|error| error.to_string())
        .and_then(|()| device.try_create_buffer_shared(bytes))?;
    buffer.zero_fill();
    Ok(buffer)
}

impl LatticeBuffers {
    fn new(device: &GpuDevice, cells: [u32; 3]) -> Result<Self, String> {
        let cell = cell_bytes(cells);
        let face = face_bytes(cells);
        let corners = cells.iter().map(|&n| u64::from(n) + 1).product::<u64>() * 4;
        let coarse = (pocket_coarse_bytes(cells) - cell) / 2;
        Ok(Self {
            cells,
            water: allocate(device, cell)?,
            phi: allocate(device, cell)?,
            rhs: allocate(device, cell)?,
            pressure: allocate(device, cell)?,
            corners: allocate(device, corners)?,
            g: allocate(device, face)?,
            a: allocate(device, face)?,
            b: allocate(device, face)?,
            f: allocate(device, face)?,
            s: allocate(device, face)?,
            v: allocate(device, face)?,
            pocket: allocate(device, cell)?,
            pocket_gate: allocate(device, POCKET_GATE_WORDS * 4)?,
            pocket_label: allocate(device, cell)?,
            pocket_sum: allocate(device, pocket_sum_bytes(cells))?,
            solve_water: allocate(device, cell)?,
            contact_water: allocate(device, cell)?,
            let_go: allocate_zeroed(device, cell)?,
            pocket_coarse: allocate(device, coarse.max(4))?,
            pocket_coarse_label: allocate(device, coarse.max(4))?,
            pocket_leader: allocate(device, cell)?,
        })
    }
}

#[derive(Clone, Copy)]
struct NarrowHistory {
    epoch: Option<u32>,
    box_min: [f32; 3],
    cell_size: f32,
    cells: [u32; 3],
    slots: u32,
    enabled: bool,
    last_tick: i32,
}

#[derive(Default)]
pub(crate) struct StepState {
    pipelines: Option<Pipelines>,
    mask_pipeline: Option<GpuComputePipeline>,
    sorter: ParticleSorter,
    solver: PressureSolver,
    solid: Option<GpuComputePipeline>,
    lattice: Option<LatticeBuffers>,
    /// Keyed on the lattice (with `lattice`) and its own `ring_max`.
    tiles: Option<TileTable>,
    /// The lattice's cell arrays and gathered faces hold canonical values
    /// everywhere (`tiles_fill` ran); false until the first step on a lattice.
    filled: bool,
    sorted: Option<GpuBuffer>,
    /// The emission flags' scan, one word a half-cell site.
    emit_scan: PrefixScan,
    /// The face grid output: exactly [`face_bytes`] of the current lattice.
    faces: Option<GpuBuffer>,
    /// [`ZERO_BYTES`] zero bytes bound where an optional input is unwired.
    saved_particles: Option<GpuBuffer>,
    saved_faces: Option<GpuBuffer>,
    saved_distance: Option<GpuBuffer>,
    saved_reaction: Option<GpuBuffer>,
    saved_capped: Option<GpuBuffer>,
    saved_narrow_phi: Option<GpuBuffer>,
    saved_narrow_faces: Option<GpuBuffer>,
    saved_narrow_failure: Option<GpuBuffer>,
    zeros: Option<GpuBuffer>,
    bodies: BodyPasses,
    clock: Option<GpuFlipClock>,
    clock_capacities: [u32; 3],
    history: crate::node_graph::liquid::substep_history::SubstepHistory,
    narrow: NarrowBand,
    narrow_history: Option<NarrowHistory>,
    narrow_reset_pending: bool,
    narrow_full_count: bool,
    interior: Option<GpuBuffer>,
}

/// Zero bytes bound for an unwired input: the uniform-sized arrays, and an
/// empty reaction for every body a liquid holds.
const ZERO_BYTES: u64 = (MAX_FLUID_ROLES * REACTION_FLOATS * 4) as u64;

const SORT_LABELS: SortLabels = SortLabels {
    clear: "gpu_flip.step.sort.clear",
    count: "gpu_flip.step.sort.count",
    scan: ScanLabels {
        blocks: "gpu_flip.step.sort.scan.blocks",
        add: "gpu_flip.step.sort.scan.add",
    },
    ranges: "gpu_flip.step.sort.ranges",
    tail: "gpu_flip.step.sort.tail",
    scatter: "gpu_flip.step.sort.scatter",
    stabilise: "gpu_flip.step.sort.stabilise",
};

fn groups(threads: u64) -> [u32; 3] {
    [(threads.div_ceil(256)).max(1) as u32, 1, 1]
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer { binding, buffer, offset: 0 }
}

fn uniform(params: &StepParams) -> GpuBinding<'_> {
    GpuBinding::Bytes {
        binding: 0,
        data: bytemuck::bytes_of(params),
    }
}

fn narrow_uniform(params: &NbParams) -> GpuBinding<'_> {
    GpuBinding::Bytes {
        binding: 0,
        data: bytemuck::bytes_of(params),
    }
}

fn narrow_params(params: &StepParams, initialized: bool) -> NbParams {
    NbParams {
        n: params.n,
        slots: params.capacity,
        minimum: params.box_min,
        h: params.cell_size,
        dt: params.step_dt,
        axis: 0,
        initialized: u32::from(initialized),
        closed_faces: params.closed_faces,
    }
}

fn narrow_redistance(enc: &mut GpuEncoder, pipes: &BandPipelines, params: &NbParams, source: &GpuBuffer, output: &GpuBuffer, cells: [u32; 3], label: &str) {
    enc.dispatch_compute(
        &pipes.seed,
        &[narrow_uniform(params), buffer(1, source), buffer(2, output)],
        groups(u64::from(cells[0]) * u64::from(cells[1]) * u64::from(cells[2])),
        label,
    );
    enc.compute_memory_barrier_buffers();
    for axis in 0..3u32 {
        let mut pass = *params;
        pass.axis = axis;
        let lines = match axis {
            0 => u64::from(cells[1]) * u64::from(cells[2]),
            1 => u64::from(cells[0]) * u64::from(cells[2]),
            _ => u64::from(cells[0]) * u64::from(cells[1]),
        };
        enc.dispatch_compute(&pipes.sweep, &[narrow_uniform(&pass), buffer(2, output)], groups(lines), label);
        enc.compute_memory_barrier_buffers();
    }
}

/// `layers` extension passes from `source` into `target`, ping-ponging
/// through `scratch` so the last pass lands in `target`. `source` may be
/// `target`: an even count's first pass writes `scratch`, an odd one starts from a copy there.
fn extend(enc: &mut GpuEncoder, pipes: &Pipelines, clock_plan: &GpuBuffer, params: &StepParams, face_groups: [u32; 3], [source, target, scratch]: [&GpuBuffer; 3], layers: u32, label: &str) {
    let mut from = source;
    // In place with an odd count, the first pass would write its own source:
    // start from a copy in `scratch` instead, which that pass does not write.
    if !layers.is_multiple_of(2) && std::ptr::eq(source, target) {
        enc.copy_buffer_to_buffer(source, scratch, source.size.min(scratch.size));
        from = scratch;
    }
    for i in 0..layers {
        let to = if (layers - i) % 2 == 1 { target } else { scratch };
        enc.dispatch_compute(&pipes.extend, &[buffer(46, clock_plan), uniform(params), buffer(3, from), buffer(4, to)], face_groups, label);
        from = to;
    }
}

/// Layers of the solid velocity's extrapolation (gpu_flip_step.wgsl
/// SOLID_LAYERS, MeshLevelSet::_numVelocityExtrapolationLayers).
const SOLID_LAYERS: u32 = 5;

/// Rounds of the sealed-pocket spread: one round of line sweeps crosses the
/// lattice along each axis, so the cap is the longest side.
pub(crate) fn pocket_rounds(cells: [u32; 3]) -> u32 {
    cells.into_iter().max().unwrap_or(1)
}

/// Which water reaches air (gpu_flip_step.wgsl pocket_*): seed, up to
/// [`pocket_rounds`] rounds of indirect sweeps that stop once a round changes
/// nothing, the unfinished check, then the tally.
fn encode_pockets(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    l: &LatticeBuffers,
    ranges: &GpuBuffer,
    cells: [u32; 3],
    capped: &GpuBuffer,
    tally: u64,
) {
    let cell_count: u64 = cells.iter().map(|&n| u64::from(n)).product();
    enc.dispatch_compute(
        &pipes.pocket_seed,
        &[uniform(params), buffer(6, &l.water), buffer(10, &l.s), buffer(23, &l.pocket), buffer(25, &l.pocket_label)],
        groups(cell_count),
        "gpu_flip.step.pocket_seed",
    );
    enc.dispatch_compute(&pipes.pocket_start, &[buffer(24, &l.pocket_gate)], [1, 1, 1], "gpu_flip.step.pocket_start");
    for _ in 0..pocket_rounds(cells) {
        enc.dispatch_compute(&pipes.pocket_round, &[uniform(params), buffer(24, &l.pocket_gate)], [1, 1, 1], "gpu_flip.step.pocket_round");
        for (axis, sweep) in pipes.pocket_sweep.iter().enumerate() {
            enc.dispatch_compute_indirect(
                sweep,
                &[uniform(params), buffer(10, &l.s), buffer(23, &l.pocket), buffer(24, &l.pocket_gate), buffer(25, &l.pocket_label)],
                &l.pocket_gate,
                12 * axis as u64,
                "gpu_flip.step.pocket_sweep",
            );
        }
    }
    enc.dispatch_compute(
        &pipes.pocket_check,
        &[uniform(params), buffer(10, &l.s), buffer(23, &l.pocket), buffer(24, &l.pocket_gate), buffer(25, &l.pocket_label)],
        groups(cell_count),
        "gpu_flip.step.pocket_check",
    );
    // Adds the step's unfinished spread to the solver word, cleared on the
    // tick's first step, and writes the step's dry, sealed and air counts.
    enc.dispatch_compute(
        &pipes.pocket_tally,
        &[uniform(params), buffer(1, ranges), buffer(6, &l.water), buffer(7, &l.phi), buffer(10, &l.s), buffer(23, &l.pocket), buffer(24, &l.pocket_gate), GpuBinding::Buffer { binding: 22, buffer: capped, offset: tally }],
        [1, 1, 1],
        "gpu_flip.step.pocket_tally",
    );
}

/// The pockets one lattice's right-hand side is corrected over: each
/// cell's sealed state and its pocket's label (a cell of that lattice), and
/// the sums, three words a cell plus the removed total's two.
struct Pockets<'a> {
    state: &'a GpuBuffer,
    label: &'a GpuBuffer,
    sums: &'a GpuBuffer,
}

impl LatticeBuffers {
    fn fine_pockets(&self) -> Pockets<'_> {
        Pockets {
            state: &self.pocket,
            label: &self.pocket_label,
            sums: &self.pocket_sum,
        }
    }

    fn coarse_pockets(&self) -> Pockets<'_> {
        Pockets {
            state: &self.pocket_coarse,
            label: &self.pocket_coarse_label,
            sums: &self.pocket_sum,
        }
    }
}

/// Each sealed pocket's mean taken off the right-hand side `rhs` on the
/// lattice `params.n` (gpu_flip_step.wgsl pocket_accumulate,
/// pocket_remove): with no air cell its solve is pure Neumann, solvable only
/// for a right-hand side summing to 0. What was removed goes to solver word
/// 4 (`solve` 0, pressure) or 5 (1, density): h³ of the lattice's cell
/// size, so a coarse level adds its own volume rate.
fn encode_pocket_mean(enc: &mut GpuEncoder, pipes: &Pipelines, params: &StepParams, pockets: Pockets<'_>, rhs: &GpuBuffer, solve: usize, capped: &GpuBuffer, tally: u64) {
    let cell_count: u64 = params.n.iter().map(|&n| u64::from(n)).product();
    let sums = [uniform(params), buffer(26, pockets.sums)];
    enc.dispatch_compute(&pipes.pocket_clear, &sums, groups(3 * cell_count + 2), "gpu_flip.step.pocket_clear");
    let cells = [uniform(params), buffer(5, rhs), buffer(23, pockets.state), buffer(25, pockets.label), buffer(26, pockets.sums)];
    enc.dispatch_compute(&pipes.pocket_accumulate, &cells, groups(cell_count), "gpu_flip.step.pocket_accumulate");
    enc.dispatch_compute(&pipes.pocket_remove, &cells, groups(cell_count), "gpu_flip.step.pocket_remove");
    enc.dispatch_compute(
        &pipes.pocket_flux[solve],
        &[
            uniform(params),
            buffer(26, pockets.sums),
            GpuBinding::Buffer {
                binding: 22,
                buffer: capped,
                offset: tally,
            },
        ],
        [1, 1, 1],
        "gpu_flip.step.pocket_flux",
    );
}

/// The pockets at solve level `params.solve_level` from the fine ones
/// (gpu_flip_step.wgsl pocket_coarsen): a level cell is sealed when every
/// fine cell under it is sealed with one label, and takes the lowest such
/// level cell of its pocket as its label.
fn encode_pocket_coarsen(enc: &mut GpuEncoder, pipes: &Pipelines, params: &StepParams, l: &LatticeBuffers) {
    let level = params.solve_level as usize;
    let coarse_count: u64 = level_lattices(l.cells)[level].iter().map(|&n| u64::from(n)).product();
    let cell_count: u64 = l.cells.iter().map(|&n| u64::from(n)).product();
    enc.dispatch_compute(
        &pipes.pocket_coarse[0],
        &[uniform(params), buffer(41, &l.pocket_leader)],
        groups(cell_count),
        "gpu_flip.step.pocket_leader_clear",
    );
    let bindings = [
        uniform(params),
        buffer(6, &l.water),
        buffer(10, &l.s),
        buffer(23, &l.pocket),
        buffer(25, &l.pocket_label),
        buffer(39, &l.pocket_coarse),
        buffer(40, &l.pocket_coarse_label),
        buffer(41, &l.pocket_leader),
    ];
    enc.dispatch_compute(&pipes.pocket_coarse[1], &bindings, groups(coarse_count), "gpu_flip.step.pocket_coarsen");
    enc.dispatch_compute(&pipes.pocket_coarse[2], &bindings, groups(coarse_count), "gpu_flip.step.pocket_relabel");
}

/// The step shader's atomic sites (I8, GPU_FLIP_PRESSURE_SOLVE.md section
/// 7 (Invariants & enforcement)): the pockets' fixed-point sums, integer adds that come out
/// the same in any thread order, and the tally's cell counts and lowest air
/// seed (an integer minimum, also order-free). A new site is added here on
/// purpose.
#[cfg(test)]
const POCKET_ATOMIC_SITES: &[&str] = &[
    "pocket_sum",
    "group_sum",
    "pocket_clear",
    "pocket_add",
    "pocket_accumulate",
    "pocket_remove",
    "pocket_flux",
    "pocket_counts",
    "pocket_first_seed",
    "pocket_dry_floor",
    "pocket_tally",
    "pocket_leader",
    "pocket_leader_clear",
    "pocket_coarsen",
    "pocket_relabel",
];

/// I8's guard: each line of `source` that uses an atomic, outside the named
/// functions and variable declarations of `allowed`; exchange and
/// compare-exchange are never allowed. Comments are skipped.
#[cfg(test)]
pub(crate) fn atomic_sites_outside(source: &str, allowed: &[&str]) -> Vec<String> {
    let mut owner = "";
    let mut stray = Vec::new();
    for line in source.lines() {
        if let Some(rest) = line.strip_prefix("fn ") {
            owner = rest.split('(').next().unwrap_or("");
        } else if let Some(at) = line.find("var<").filter(|_| !line.starts_with(' ')) {
            owner = line[at..].split_once("> ").map_or("", |(_, rest)| rest.split(':').next().unwrap_or("").trim());
        }
        let code = line.split("//").next().unwrap_or("");
        if !code.contains("atomic") {
            continue;
        }
        if code.contains("atomicExchange") || code.contains("atomicCompareExchange") || !allowed.contains(&owner) {
            stray.push(format!("{owner}: {}", line.trim()));
        }
    }
    stray
}

/// Everything one step reads, resolved before any pass is encoded.
#[derive(Clone, Copy)]
struct Step<'a> {
    params: StepParams,
    clock_plan: &'a GpuBuffer,
    particles: &'a GpuBuffer,
    out: &'a GpuBuffer,
    /// Two words a slot: guarded RK3 stages and refused push-outs; then the
    /// tick's solver words (liquid_stats.rs SOLVER_WORDS) at byte `tally`.
    capped: &'a GpuBuffer,
    tally: u64,
    count: u32,
    forces: &'a GpuBuffer,
    impulses: &'a GpuBuffer,
    bodies: &'a GpuBuffer,
    shapes: &'a GpuBuffer,
    atlas: &'a GpuBuffer,
    /// Inflow and outflow rows; read when `params.region_count` > 0.
    regions: &'a GpuBuffer,
    /// What the water has pushed on each body so far this tick, read by the
    /// solid velocity; added to when `dynamic`.
    reaction: &'a GpuBuffer,
    /// The bodies take part in the pressure solve and gather its reaction.
    dynamic: bool,
    /// When the pressure and density solves stop.
    pressure: Stop,
    /// The V-cycle level both solves' gradients run on; 0 is the fine
    /// lattice.
    level: usize,
    band: u32,
    ghost: bool,
    /// Run the density projection.
    density: bool,
    narrow_enabled: bool,
    restore_narrow: bool,
}

impl StepState {
    /// Build every pipeline a step dispatches, at install.
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        if self.pipelines.is_none() {
            self.pipelines = Some(Pipelines::new(device));
        }
        if self.mask_pipeline.is_none() {
            self.mask_pipeline = Some(device.create_compute_pipeline(
                MASK_SHADER,
                "commit_mask",
                "gpu_flip.commit_mask",
            ));
        }
        self.sorter.prepare(device);
        self.emit_scan.prepare(device);
        self.solver.prepare_pipelines(device);
        self.bodies.prepare_pipelines(device);
        self.narrow.prepare(device);
    }

    /// Size every array for `cells` and `slots` before anything is encoded;
    /// `sources` adds the emission scan; removal reuses the sorted array.
    fn reserve(&mut self, device: &GpuDevice, cells: [u32; 3], slots: u64, ring_max: u32, sources: bool, narrow_enabled: bool, interior_wired: bool) -> Result<(), String> {
        if self.zeros.is_none() {
            let zeros = device.try_create_buffer_shared(ZERO_BYTES)?;
            zeros.zero_fill();
            self.zeros = Some(zeros);
        }
        let saved_particle_bytes = slots.max(1) * size_of::<FluidParticle>() as u64;
        if self
            .saved_particles
            .as_ref()
            .is_none_or(|buffer| buffer.size < saved_particle_bytes)
        {
            self.saved_particles = Some(allocate(device, saved_particle_bytes)?);
        }
        let saved_face_bytes = face_bytes(cells);
        if self
            .saved_faces
            .as_ref()
            .is_none_or(|buffer| buffer.size < saved_face_bytes)
        {
            self.saved_faces = Some(allocate(device, saved_face_bytes)?);
        }
        let saved_distance_bytes = cell_bytes(cells);
        if self
            .saved_distance
            .as_ref()
            .is_none_or(|buffer| buffer.size < saved_distance_bytes)
        {
            self.saved_distance = Some(allocate(device, saved_distance_bytes)?);
        }
        if self
            .saved_reaction
            .as_ref()
            .is_none_or(|buffer| buffer.size < ZERO_BYTES)
        {
            self.saved_reaction = Some(allocate(device, ZERO_BYTES)?);
        }
        let saved_capped_bytes = slots.max(1) * 8 + u64::from(SOLVER_WORDS) * 4;
        if self
            .saved_capped
            .as_ref()
            .is_none_or(|buffer| buffer.size < saved_capped_bytes)
        {
            self.saved_capped = Some(allocate(device, saved_capped_bytes)?);
        }
        self.sorter.reserve_ranges(device, cells)?;
        if self.lattice.as_ref().is_none_or(|l| l.cells != cells) {
            self.lattice = None;
            self.tiles = None;
            self.filled = false;
            self.lattice = Some(LatticeBuffers::new(device, cells)?);
        }
        if self.tiles.as_ref().is_none_or(|t| t.ring_max != ring_max) {
            self.tiles = None;
            self.tiles = Some(TileTable::new(device, cells, ring_max)?);
        }
        let face = face_bytes(cells);
        if self.faces.as_ref().is_none_or(|faces| faces.size != face) {
            self.faces = None;
            let faces = crate::node_graph::scene_modifier_expand::admit_candidate_bytes(device.modifier_memory_snapshot(), face)
                .map_err(|error| error.to_string())
                .and_then(|()| device.try_create_buffer_shared(face))?;
            // A consumer reading before the first step sees still, invalid faces.
            faces.zero_fill();
            self.faces = Some(faces);
        }
        let sorted = slots.max(1) * size_of::<FluidParticle>() as u64;
        if self.sorted.as_ref().is_none_or(|buffer| buffer.size < sorted) {
            self.sorted = None;
            self.sorted = Some(allocate(device, sorted)?);
        }
        if sources {
            self.emit_scan.buffer(device, emit_sites(cells) as usize)?;
        }
        if narrow_enabled {
            let slots = u32::try_from(slots).map_err(|_| "narrow-band particle capacity exceeds u32".to_string())?;
            self.narrow.reserve(device, cells, slots)?;
            for (saved, bytes) in [
                (&mut self.saved_narrow_phi, cell_bytes(cells)),
                (&mut self.saved_narrow_faces, face_bytes(cells)),
                (&mut self.saved_narrow_failure, 4),
            ] {
                if saved.as_ref().is_none_or(|buffer| buffer.size != bytes) {
                    *saved = Some(allocate(device, bytes)?);
                }
            }
        }
        if !interior_wired {
            self.interior = None;
        } else {
            let bytes = cell_bytes(cells);
            if self.interior.as_ref().is_none_or(|buffer| buffer.size != bytes) {
                self.interior = Some(allocate(device, bytes)?);
            }
        }
        Ok(())
    }

    fn  commit_mask(
        &self,
        enc: &mut GpuEncoder,
        plan: &GpuBuffer,
        target: &GpuBuffer,
        saved: &GpuBuffer,
        label: &'static str,
    ) {
        let words = (target.size.min(saved.size) / 4).min(u64::from(u32::MAX)) as u32;
        if words == 0 {
            return;
        }
        let params = [words, 0, 0, 0];
        enc.dispatch_compute(
            self.mask_pipeline.as_ref().expect("mask pipeline prepared"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                buffer(1, plan),
                buffer(2, target),
                buffer(3, saved),
            ],
            groups(u64::from(words)),
            label,
        );
        enc.compute_memory_barrier_buffers();
    }

    /// Update the narrow-band identity before a step.  Geometry, capacity,
    /// epoch and an unwired epoch's tick rollback all invalidate its distance
    /// history.  A plain enable toggle is handled separately so disabling can
    /// restore the held interior before dense FLIP resumes.
    fn sync_narrow_history(&mut self, epoch: Option<u32>, box_min: [f32; 3], cell_size: f32, cells: [u32; 3], slots: u32, tick: i32, enabled: bool) -> bool {
        let previous = self.narrow_history;
        let identity_changed = previous.is_none_or(|history| {
            history.epoch != epoch
                || history.box_min.map(f32::to_bits) != box_min.map(f32::to_bits)
                || history.cell_size.to_bits() != cell_size.to_bits()
                || history.cells != cells
                || history.slots != slots
                || (epoch.is_none() && tick < history.last_tick)
        });
        if identity_changed {
            self.narrow.initialized = false;
            self.narrow_reset_pending = true;
            self.narrow_full_count = false;
        }
        let restore = !enabled && previous.is_some_and(|history| history.enabled) && !identity_changed;
        if !enabled && previous.is_some_and(|history| history.enabled) {
            self.narrow.initialized = false;
        }
        self.narrow_history = Some(NarrowHistory {
            epoch,
            box_min,
            cell_size,
            cells,
            slots,
            enabled,
            last_tick: tick,
        });
        restore
    }

    fn encode(&mut self, device: &GpuDevice, enc: &mut GpuEncoder, step: &Step<'_>, clock_params: &GpuFlipClockParams) -> Result<(), String> {
        let pipes = self.pipelines.as_ref().expect("step pipelines built by prepare_pipelines at install");
        let (Some(l), Some(tiles), Some(sorted), Some(out_faces)) = (self.lattice.as_ref(), self.tiles.as_ref(), self.sorted.as_ref(), self.faces.as_ref()) else {
            return Err("the step's storage was not reserved".into());
        };
        let cells = l.cells;
        let p = step.params;
        let capacity = p.capacity;
        let sort_job = |particles, sorted| SortJob {
            particles,
            read: LIQUID_PARTICLE_READ,
            capacity,
            count: step.count,
            bin_min: p.box_min,
            inv_cell: 1.0 / p.cell_size,
            bins: cells,
            sorted: Some(sorted),
            order: None,
        };
        // Copies of the params, so each Bytes binding borrows a value that
        // lives across its dispatch.  Narrow mode 1 is the one-time full
        // history initialization; mode 2 is the masked steady state.
        let narrow_initial = step.narrow_enabled && !self.narrow.initialized;
        let base = StepParams {
            particles: if step.narrow_enabled { capacity } else { p.particles },
            narrow_band: if step.narrow_enabled { if narrow_initial { 1 } else { 2 } } else { 0 },
            ..p
        };
        let nb_params = narrow_params(&base, self.narrow.initialized);
        if self.narrow_reset_pending && let Some(nb) = self.narrow.buffers.as_ref() {
            enc.clear_buffer(&nb.failure);
            self.narrow_reset_pending = false;
        }
        let corners = cells.map(|n| n + 1);
        // The box walls are in the solid with the bodies, as the engine's
        // inverted domain object is: a body flush with a wall then seals
        // against it, where the body's own lattice distance alone would leave
        // a sub-cell open channel between them. The engine's domain object
        // covers all six faces whichever are open, so this mask stays 63.
        // First, since emission seeds only outside the solid.
        encode_solid_distance(
            &mut self.solid,
            device,
            enc,
            &SolidDistanceJob {
                min: p.box_min,
                cell_size: p.cell_size,
                nodes: corners,
                closed_faces: 63,
                wall_inset: 0.0,
                body_count: p.body_count,
                rows: p.rows,
                tick_seconds: p.tick_seconds,
                bodies: step.bodies,
                shapes: step.shapes,
                atlas: step.atlas,
                out: &l.corners,
                clock_plan: step.clock_plan,
            },
            "gpu_flip.step.solid_distance",
        );
        self.sorter.encode(device, enc, &sort_job(step.particles, sorted), &SORT_LABELS)?;
        let cell_count: u64 = cells.iter().map(|&n| u64::from(n)).product();
        let face_count: u64 = cells.iter().map(|&n| u64::from(n) + 1).product();
        let ghost = StepParams { ghost: u32::from(step.ghost), ..base };
        let cells_groups = groups(cell_count);
        let face_groups = groups(face_count);

        if step.narrow_enabled {
            let nb = self.narrow.buffers.as_ref().ok_or("narrow-band storage was not reserved")?;
            let nb_pipes = self.narrow.pipes.as_ref().ok_or("narrow-band pipelines were not prepared")?;
            let ranges = self.sorter.ranges().ok_or("the cell ranges were not reserved")?;
            if narrow_initial {
                enc.dispatch_compute(
                    &pipes.band_distance,
                    &[uniform(&base), buffer(1, ranges), buffer(2, sorted), buffer(5, &l.phi), buffer(45, &nb.mask)],
                    cells_groups,
                    "gpu_flip.step.narrow.initialize_distance",
                );
                enc.dispatch_compute(
                    &pipes.band_gather,
                    &[uniform(&base), buffer(1, ranges), buffer(2, sorted), buffer(4, &l.g), buffer(45, &nb.mask)],
                    face_groups,
                    "gpu_flip.step.narrow.initialize_gather",
                );
                enc.compute_memory_barrier_buffers();
                extend(enc, pipes, step.clock_plan, &base, face_groups, [&l.g, &nb.previous_faces, &l.b], step.band, "gpu_flip.step.narrow.initialize_faces");
                narrow_redistance(enc, nb_pipes, &nb_params, &l.phi, &nb.previous_phi, cells, "gpu_flip.step.narrow.initialize_distance");
                enc.copy_buffer_to_buffer(&nb.previous_phi, &l.phi, l.phi.size);
            } else {
                enc.copy_buffer_to_buffer(&nb.phi, &nb.previous_phi, nb.phi.size);
            }
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &nb_pipes.advect_phi,
                &[narrow_uniform(&nb_params), buffer(46, step.clock_plan), buffer(1, &nb.previous_phi), buffer(2, &nb.advected_phi), buffer(3, &nb.previous_faces)],
                cells_groups,
                "gpu_flip.step.narrow.advect_phi",
            );
            enc.dispatch_compute(
                &nb_pipes.advect_faces,
                &[narrow_uniform(&nb_params), buffer(46, step.clock_plan), buffer(1, &nb.previous_phi), buffer(3, &nb.previous_faces), buffer(4, &nb.advected_faces)],
                face_groups,
                "gpu_flip.step.narrow.advect_faces",
            );
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &pipes.narrow_move,
                &[
                    uniform(&base), buffer(46, step.clock_plan),
                    buffer(2, sorted),
                    buffer(3, &nb.previous_faces),
                    buffer(9, &l.corners),
                    buffer(17, &nb.previous_faces),
                    buffer(18, &nb.previous_faces),
                    buffer(19, &nb.particles),
                    buffer(22, step.capped),
                ],
                groups(u64::from(capacity)),
                "gpu_flip.step.narrow.move",
            );
            enc.compute_memory_barrier_buffers();
            self.sorter.encode(
                device,
                enc,
                &SortJob {
                    count: capacity,
                    ..sort_job(&nb.particles, sorted)
                },
                &SORT_LABELS,
            )?;
        }
        let ranges = self.sorter.ranges().ok_or("the cell ranges were not reserved")?;

        if step.restore_narrow {
            let nb = self.narrow.buffers.as_ref().ok_or("narrow-band restore has no storage")?;
            let nb_pipes = self.narrow.pipes.as_ref().ok_or("narrow-band pipelines were not prepared")?;
            let scan_threads = cell_count.checked_mul(8).ok_or("narrow-band scan extent overflow")?;
            let scan_words = usize::try_from(scan_threads).map_err(|_| "narrow-band scan extent cannot be represented".to_string())?;
            let scan = self.narrow.scan.buffer(device, scan_words)?.clone();
            enc.copy_buffer_to_buffer(sorted, &nb.particles, nb.particles.size);
            let restore_params = narrow_params(&base, true);
            enc.dispatch_compute(
                &nb_pipes.restore_flags,
                &[
                    narrow_uniform(&restore_params), buffer(46, step.clock_plan),
                    buffer(1, &nb.phi),
                    buffer(6, &l.corners),
                    buffer(8, &nb.previous_phi),
                    buffer(9, &nb.particles),
                    buffer(10, ranges),
                    buffer(11, &scan),
                ],
                cells_groups,
                "gpu_flip.step.narrow.restore_flags",
            );
            enc.compute_memory_barrier_buffers();
            self.narrow.scan.encode_labelled(
                enc,
                scan_words,
                ScanLabels {
                    blocks: "gpu_flip.step.narrow.restore_scan.blocks",
                    add: "gpu_flip.step.narrow.restore_scan.add",
                },
            );
            enc.dispatch_compute(
                &nb_pipes.status,
                &[narrow_uniform(&restore_params), buffer(46, step.clock_plan), buffer(10, ranges), buffer(11, &scan), buffer(12, &nb.status)],
                [1, 1, 1],
                "gpu_flip.step.narrow.restore_status",
            );
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &pipes.narrow_latch,
                &[narrow_uniform(&restore_params), buffer(46, step.clock_plan), buffer(43, &nb.status), buffer(44, &nb.failure)],
                [1, 1, 1],
                "gpu_flip.step.narrow.restore_latch",
            );
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &nb_pipes.write,
                &[
                    narrow_uniform(&restore_params), buffer(46, step.clock_plan),
                    buffer(3, &nb.previous_faces),
                    buffer(9, &nb.particles),
                    buffer(10, ranges),
                    buffer(11, &scan),
                    buffer(12, &nb.status),
                ],
                groups(scan_threads),
                "gpu_flip.step.narrow.restore_write",
            );
            enc.compute_memory_barrier_buffers();
            self.sorter.encode(
                device,
                enc,
                &SortJob {
                    count: capacity,
                    ..sort_job(&nb.particles, sorted)
                },
                &SORT_LABELS,
            )?;
            self.narrow_full_count = true;
        }
        let ranges = self.sorter.ranges().ok_or("the cell ranges were not reserved")?;

        // Which tiles hold water this step, from the sort's bin counts. The
        // cell passes run over the cell set C through its indirect triple;
        // every other tile holds the canonical values (design section 4 (The
        // defined-value rule)): filled once per lattice, restored by the
        // retire when a tile leaves C.
        if !step.narrow_enabled {
            encode_tiles(enc, pipes, &base, tiles, ranges, step.capped, step.tally);
        }
        if !step.narrow_enabled && !self.filled {
            enc.dispatch_compute(&pipes.tiles_fill, &canonical(l, vec![uniform(&base)]), face_groups, "gpu_flip.step.tiles.fill");
            self.filled = true;
        }
        if !step.narrow_enabled {
            enc.dispatch_compute_indirect(
                &pipes.tiles_retire,
                &canonical(l, vec![uniform(&base), buffer(32, &tiles.retired)]),
                &tiles.args,
                retired_args_offset(tiles.ring_max),
                "gpu_flip.step.tiles.retire",
            );
        }
        #[cfg(all(test, feature = "gpu-proofs"))]
        if POISON.load(std::sync::atomic::Ordering::SeqCst) {
            enc.dispatch_compute(
                &pipes.poison,
                &canonical(l, vec![uniform(&base), buffer(28, &tiles.rank), buffer(30, &tiles.counts)]),
                cells_groups,
                "gpu_flip.step.tiles.poison",
            );
        }
        let over_c = |enc: &mut GpuEncoder, pipeline: &GpuComputePipeline, bindings: Vec<GpuBinding<'_>>, threads: [u32; 3], label: &str| {
            if step.narrow_enabled {
                let mut bound = vec![uniform(&base), buffer(46, step.clock_plan)];
                bound.extend(bindings);
                enc.dispatch_compute(pipeline, &bound, threads, label);
            } else {
                let mut bound = vec![uniform(&base), buffer(29, &tiles.by_ring), buffer(46, step.clock_plan)];
                bound.extend(bindings);
                enc.dispatch_compute_indirect(pipeline, &bound, &tiles.args, 0, label);
            }
        };
        // The water mask is φ < 0, so φ is built every step.
        let solids = p.body_count > 0;
        if step.narrow_enabled {
            let nb = self.narrow.buffers.as_ref().ok_or("narrow-band storage was not reserved")?;
            let nb_pipes = self.narrow.pipes.as_ref().ok_or("narrow-band pipelines were not prepared")?;
            enc.dispatch_compute(
                &nb_pipes.distance_support,
                &[narrow_uniform(&nb_params), buffer(46, step.clock_plan), buffer(1, &nb.advected_phi), buffer(6, &l.corners), buffer(7, &nb.mask), buffer(10, ranges)],
                cells_groups,
                "gpu_flip.step.narrow.support",
            );
            enc.compute_memory_barrier_buffers();
            over_c(
                enc,
                &pipes.band_distance,
                vec![buffer(1, ranges), buffer(2, sorted), buffer(5, &l.phi), buffer(45, &nb.mask)],
                cells_groups,
                "gpu_flip.step.narrow.distance",
            );
            enc.dispatch_compute(
                &nb_pipes.union,
                &[narrow_uniform(&nb_params), buffer(46, step.clock_plan), buffer(1, &nb.advected_phi), buffer(2, &nb.union_phi), buffer(5, &l.phi)],
                cells_groups,
                "gpu_flip.step.narrow.union",
            );
            enc.compute_memory_barrier_buffers();
            narrow_redistance(enc, nb_pipes, &nb_params, &nb.union_phi, &nb.phi, cells, "gpu_flip.step.narrow.redistance");
            enc.copy_buffer_to_buffer(&nb.phi, &l.phi, nb.phi.size);
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &nb_pipes.mask,
                &[narrow_uniform(&nb_params), buffer(46, step.clock_plan), buffer(1, &nb.phi), buffer(6, &l.corners), buffer(7, &nb.mask)],
                cells_groups,
                "gpu_flip.step.narrow.mask",
            );
            enc.compute_memory_barrier_buffers();
            over_c(
                enc,
                &pipes.band_gather,
                vec![buffer(1, ranges), buffer(2, sorted), buffer(4, &l.g), buffer(45, &nb.mask)],
                face_groups,
                "gpu_flip.step.narrow.gather",
            );
        } else {
            over_c(
                enc,
                &pipes.distance,
                vec![buffer(1, ranges), buffer(2, sorted), buffer(5, &l.phi)],
                cells_groups,
                "gpu_flip.step.distance",
            );
            over_c(
                enc,
                &pipes.gather,
                vec![buffer(1, ranges), buffer(2, sorted), buffer(4, &l.g)],
                face_groups,
                "gpu_flip.step.particles_to_faces",
            );
        }
        // The saved faces: the particles' own, extended so FLIP's change is
        // measured wherever a particle samples.
        if step.narrow_enabled {
            extend(enc, pipes, step.clock_plan, &base, face_groups, [&l.g, &l.a, &l.b], step.band, "gpu_flip.step.narrow.extend_old");
            let nb = self.narrow.buffers.as_ref().ok_or("narrow-band storage was not reserved")?;
            let nb_pipes = self.narrow.pipes.as_ref().ok_or("narrow-band pipelines were not prepared")?;
            enc.dispatch_compute(
                &nb_pipes.combine,
                &[narrow_uniform(&nb_params), buffer(46, step.clock_plan), buffer(1, &nb.phi), buffer(3, &nb.advected_faces), buffer(4, &l.g), buffer(6, &l.corners)],
                face_groups,
                "gpu_flip.step.narrow.combine",
            );
            extend(enc, pipes, step.clock_plan, &base, face_groups, [&l.g, &l.g, &l.b], step.band, "gpu_flip.step.narrow.extend_combined");
        } else {
            extend(enc, pipes, step.clock_plan, &base, face_groups, [&l.g, &l.a, &l.b], step.band, "gpu_flip.step.extend_old");
        }
        let force_faces = if step.narrow_enabled { &l.g } else { &l.a };
        enc.dispatch_compute(
            &pipes.gravity,
            &[
                uniform(&base),
                buffer(3, force_faces),
                buffer(4, &l.f),
                buffer(12, step.forces),
                buffer(13, step.impulses),
                buffer(15, step.shapes),
                buffer(16, step.atlas),
                buffer(36, step.regions),
                buffer(46, step.clock_plan),
            ],
            face_groups,
            "gpu_flip.step.forces",
        );
        enc.dispatch_compute(&pipes.open, &[buffer(46, step.clock_plan), uniform(&base), buffer(9, &l.corners), buffer(4, &l.s)], face_groups, "gpu_flip.step.open_fractions");
        enc.dispatch_compute(
            &pipes.solid_velocity,
            &[
                uniform(&base),
                buffer(10, &l.s),
                buffer(4, &l.b),
                buffer(14, step.bodies),
                buffer(15, step.shapes),
                buffer(16, step.atlas),
                buffer(21, step.reaction),
                buffer(46, step.clock_plan),
            ],
            face_groups,
            "gpu_flip.step.solid_velocity",
        );
        // The samples carried out over the open faces, every solid alike
        // (meshlevelset.cpp normalizeVelocityGrid): an odd layer count from
        // the scratch ends in `l.v`.
        const _: () = assert!(SOLID_LAYERS % 2 == 1);
        for layer in 0..SOLID_LAYERS {
            let (from, to) = if layer % 2 == 0 { (&l.b, &l.v) } else { (&l.v, &l.b) };
            enc.dispatch_compute(
                &pipes.solid_extrapolate,
                &[uniform(&base), buffer(3, from), buffer(4, to)],
                face_groups,
                "gpu_flip.step.solid_extrapolate",
            );
        }
        if solids {
            over_c(
                enc,
                &pipes.phi_into_solids,
                vec![buffer(9, &l.corners), buffer(5, &l.phi)],
                cells_groups,
                "gpu_flip.step.phi_into_solids",
            );
        }
        over_c(enc, &pipes.water_from_phi, vec![buffer(7, &l.phi), buffer(5, &l.water)], cells_groups, "gpu_flip.step.water_from_phi");
        // Which water reaches air holds every step: the density source reads
        // it too. As the engine does, the solid velocity's zeroing is skipped
        // when the bodies are in the solve: their mass resolves the pocket.
        encode_pockets(enc, pipes, &base, l, ranges, cells, step.capped, step.tally);
        enc.dispatch_compute(
            &pipes.pocket_pin,
            &[uniform(&base), buffer(6, &l.water), buffer(23, &l.pocket), buffer(25, &l.pocket_label), buffer(5, &l.solve_water)],
            cells_groups,
            "gpu_flip.step.pocket_pin",
        );
        // Separating solids (GPU_FLIP_PRESSURE_SOLVE.md section 8): the
        // let-go set from the last step's update comes out of the main
        // solve's mask, its cells held at pressure 0.
        let separate = separating();
        let main_water = if separate {
            enc.dispatch_compute(
                &pipes.separate_pin,
                &[uniform(&base), buffer(46, step.clock_plan), buffer(6, &l.solve_water), buffer(10, &l.s), buffer(42, &l.let_go), buffer(5, &l.contact_water)],
                cells_groups,
                "gpu_flip.step.separate_pin",
            );
            &l.contact_water
        } else {
            &l.solve_water
        };
        if step.level > 0 {
            encode_pocket_coarsen(enc, pipes, &base, l);
        }
        if solids && !step.dynamic {
            enc.dispatch_compute(
                &pipes.pocket_condition,
                &[uniform(&base), buffer(4, &l.v), buffer(10, &l.s), buffer(23, &l.pocket)],
                face_groups,
                "gpu_flip.step.pocket_condition",
            );
        }
        over_c(
            enc,
            &pipes.divergence,
            vec![buffer(3, &l.f), buffer(5, &l.rhs), buffer(6, &l.water), buffer(10, &l.s), buffer(11, &l.v)],
            cells_groups,
            "gpu_flip.step.divergence",
        );
        encode_pocket_mean(enc, pipes, &base, l.fine_pockets(), &l.rhs, 0, step.capped, step.tally);
        // At a coarse solve level the restricted right-hand side loses each
        // pocket's zero sum (design section 11), so the mean comes off once
        // more there; as a later substep's would, its flux adds to the word.
        let coarse_mean = |solve: usize| {
            move |enc: &mut GpuEncoder, rhs: &GpuBuffer, lattice: [u32; 3], h: f32| {
                let params = StepParams {
                    n: lattice,
                    cell_size: h,
                    step_in_tick: 1,
                    ..base
                };
                encode_pocket_mean(enc, pipes, &params, l.coarse_pockets(), rhs, solve, step.capped, step.tally);
            }
        };
        let water = Water {
            lattice: cells,
            cell_size: p.cell_size,
            water: main_water,
            faces: &l.s,
            phi: step.ghost.then_some(&l.phi),
        };
        self.solver.prepare(device, enc, &water)?;
        // Dynamic bodies join the solve as the engine's mass-aware PCG
        // (RigidFluidCoupling) has them; their tick rows start at `first`.
        let coupled = Bodies {
            lattice: cells,
            lattice_min: p.box_min,
            cell_size: p.cell_size,
            density: WATER_DENSITY,
            tick_seconds: p.tick_seconds,
            first: (p.rows - p.body_count).max(0) as u32,
            count: p.body_count.max(0) as u32,
            water: main_water,
            open: &l.s,
            solid: &l.v,
            bodies: step.bodies,
        };
        self.bodies.set_clock_plan(step.clock_plan);
        if step.dynamic {
            self.bodies.prepare(device, cells, coupled.count)?;
            #[cfg(all(test, feature = "gpu-proofs"))]
            if POISON.load(std::sync::atomic::Ordering::SeqCst) {
                self.bodies.poison(enc, &coupled);
            }
        }
        let passes = step.dynamic.then_some((&self.bodies, &coupled));
        let pressure_mean = coarse_mean(0);
        self.solver.solve(
            enc,
            &water,
            Solve {
                rhs: &l.rhs,
                pressure: &l.pressure,
                stop: step.pressure,
                bodies: passes,
                level: step.level,
                coarse_rhs: Some(&pressure_mean),
            },
        )?;
        let tally = step.tally;
        self.solver.tally(enc, step.pressure, step.capped, tally, 0, step.params.step_in_tick == 0)?;
        // φ binds the water array when the ghost rows are off; the pass never reads it then.
        let phi = if step.ghost { &l.phi } else { &l.water };
        let subtract = |enc: &mut GpuEncoder, params: &StepParams, phi: &GpuBuffer, faces: &GpuBuffer, label: &str| {
            enc.dispatch_compute(
                &pipes.subtract,
                &[uniform(params), buffer(46, step.clock_plan), buffer(20, faces), buffer(10, &l.s), buffer(6, &l.water), buffer(8, &l.pressure), buffer(7, phi)],
                face_groups,
                label,
            );
        };
        subtract(enc, &ghost, phi, &l.f, "gpu_flip.step.project");
        // The engine's finishPressure: the pressure's impulse goes to the
        // bodies and their velocity change to the solid faces. The
        // constraint's friction below acts on the water only.
        if step.dynamic {
            self.bodies.react(enc, &coupled, self.solver.tiles()?, &l.pressure, step.reaction)?;
        }
        extend(enc, pipes, step.clock_plan, &base, face_groups, [&l.f, out_faces, &l.b], step.band, "gpu_flip.step.extend_new");
        enc.compute_memory_barrier_buffers();
        // The engine constrains its velocity and its saved velocity to the
        // solids after the pressure solve, so FLIP's change is measured
        // between two constrained fields.
        for (faces, label) in [(out_faces, "gpu_flip.step.constrain"), (&l.a, "gpu_flip.step.constrain_old")] {
            enc.dispatch_compute(&pipes.constrain, &[buffer(46, step.clock_plan), uniform(&base), buffer(20, faces), buffer(10, &l.s), buffer(11, &l.v)], face_groups, label);
        }
        enc.copy_buffer_to_buffer(out_faces, &l.f, out_faces.size);
        // One active-set update for the next step. The leftover divergence
        // is the divergence pass itself on the projected, constrained faces,
        // against the solid velocity after the reaction, so a body's own
        // motion counts exactly as it does in the right-hand side. The
        // right-hand side is free until the density source rewrites it.
        if separate {
            over_c(
                enc,
                &pipes.divergence,
                vec![buffer(3, &l.f), buffer(5, &l.rhs), buffer(6, &l.water), buffer(10, &l.s), buffer(11, &l.v)],
                cells_groups,
                "gpu_flip.step.separate_divergence",
            );
            enc.dispatch_compute(
                &pipes.separate_update,
                &[
                    uniform(&base),
                    buffer(46, step.clock_plan),
                    buffer(6, &l.contact_water),
                    buffer(5, &l.rhs),
                    buffer(8, &l.pressure),
                    buffer(10, &l.s),
                    buffer(42, &l.let_go),
                ],
                cells_groups,
                "gpu_flip.step.separate_update",
            );
        }
        // The density projection (module doc): its pressure's gradient is
        // taken off a copy of the new faces in `l.f`, and the move reads the
        // difference as a displacement. Air sits at zero at its centres.
        let spread = if step.density {
            if step.narrow_enabled {
                let nb = self.narrow.buffers.as_ref().ok_or("narrow-band storage was not reserved")?;
                enc.dispatch_compute(
                    &pipes.band_density,
                    &[
                        uniform(&base),
                        buffer(1, ranges),
                        buffer(2, sorted),
                        buffer(6, &l.water),
                        buffer(9, &l.corners),
                        buffer(5, &l.rhs),
                        buffer(45, &nb.mask),
                        buffer(46, step.clock_plan),
                    ],
                    cells_groups,
                    "gpu_flip.step.narrow.density_source",
                );
            } else {
                over_c(
                    enc,
                    &pipes.density,
                    vec![buffer(1, ranges), buffer(2, sorted), buffer(6, &l.water), buffer(9, &l.corners), buffer(5, &l.rhs)],
                    cells_groups,
                    "gpu_flip.step.density_source",
                );
            }
            encode_pocket_mean(enc, pipes, &base, l.fine_pockets(), &l.rhs, 1, step.capped, step.tally);
            // The density solve stays plain: every water cell in it, let go
            // or not, so its rows are rebuilt on the pocket-only mask.
            let plain_water = Water { water: &l.solve_water, ..water };
            if separate {
                self.solver.prepare(device, enc, &plain_water)?;
            }
            let flat = Water { phi: None, ..plain_water };
            let density_mean = coarse_mean(1);
            self.solver.solve(
                enc,
                &flat,
                Solve {
                    rhs: &l.rhs,
                    pressure: &l.pressure,
                    stop: step.pressure,
                    bodies: None,
                    level: step.level,
                    coarse_rhs: Some(&density_mean),
                },
            )?;
            self.solver.tally(enc, step.pressure, step.capped, tally, 1, false)?;
            let plain = StepParams { ghost: 0, ..base };
            subtract(enc, &plain, &l.water, &l.f, "gpu_flip.step.density_project");
            extend(enc, pipes, step.clock_plan, &base, face_groups, [&l.f, &l.f, &l.b], step.band, "gpu_flip.step.extend_spread");
            &l.f
        } else {
            out_faces
        };
        enc.dispatch_compute(
            &pipes.advect,
            &[
                uniform(&base),
                buffer(2, sorted),
                buffer(9, &l.corners),
                buffer(3, out_faces),
                buffer(17, &l.a),
                buffer(18, spread),
                buffer(19, step.out),
                buffer(22, step.capped),
                buffer(15, step.shapes),
                buffer(16, step.atlas),
                buffer(36, step.regions),
                buffer(46, step.clock_plan),
            ],
            groups(u64::from(base.particles)),
            "gpu_flip.step.move",
        );
        if step.narrow_enabled {
            let nb = self.narrow.buffers.as_ref().ok_or("narrow-band storage was not reserved")?;
            let nb_pipes = self.narrow.pipes.as_ref().ok_or("narrow-band pipelines were not prepared")?;
            enc.dispatch_compute(
                &nb_pipes.delete,
                &[narrow_uniform(&nb_params), buffer(46, step.clock_plan), buffer(1, &nb.phi), buffer(6, &l.corners), buffer(9, step.out)],
                groups(u64::from(capacity)),
                "gpu_flip.step.narrow.delete",
            );
            enc.compute_memory_barrier_buffers();
            self.sorter.encode(
                device,
                enc,
                &SortJob {
                    count: capacity,
                    ..sort_job(step.out, &nb.particles)
                },
                &SORT_LABELS,
            )?;
            let ranges = self.sorter.ranges().ok_or("the cell ranges were not reserved")?;
            let scan_threads = cell_count.checked_mul(8).ok_or("narrow-band scan extent overflow")?;
            let scan_words = usize::try_from(scan_threads).map_err(|_| "narrow-band scan extent cannot be represented".to_string())?;
            let scan = self.narrow.scan.buffer(device, scan_words)?.clone();
            enc.dispatch_compute(
                &nb_pipes.flags,
                &[
                    narrow_uniform(&nb_params), buffer(46, step.clock_plan),
                    buffer(1, &nb.phi),
                    buffer(6, &l.corners),
                    buffer(8, &nb.previous_phi),
                    buffer(9, &nb.particles),
                    buffer(10, ranges),
                    buffer(11, &scan),
                ],
                cells_groups,
                "gpu_flip.step.narrow.reseed_flags",
            );
            enc.compute_memory_barrier_buffers();
            self.narrow.scan.encode_labelled(
                enc,
                scan_words,
                ScanLabels {
                    blocks: "gpu_flip.step.narrow.reseed_scan.blocks",
                    add: "gpu_flip.step.narrow.reseed_scan.add",
                },
            );
            enc.dispatch_compute(
                &nb_pipes.status,
                &[narrow_uniform(&nb_params), buffer(46, step.clock_plan), buffer(10, ranges), buffer(11, &scan), buffer(12, &nb.status)],
                [1, 1, 1],
                "gpu_flip.step.narrow.reseed_status",
            );
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &pipes.narrow_latch,
                &[narrow_uniform(&nb_params), buffer(46, step.clock_plan), buffer(43, &nb.status), buffer(44, &nb.failure)],
                [1, 1, 1],
                "gpu_flip.step.narrow.reseed_latch",
            );
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &nb_pipes.write,
                &[
                    narrow_uniform(&nb_params), buffer(46, step.clock_plan),
                    buffer(3, out_faces),
                    buffer(9, &nb.particles),
                    buffer(10, ranges),
                    buffer(11, &scan),
                    buffer(12, &nb.status),
                ],
                groups(scan_threads),
                "gpu_flip.step.narrow.reseed_write",
            );
            enc.compute_memory_barrier_buffers();
            self.sorter.encode(
                device,
                enc,
                &SortJob {
                    count: capacity,
                    ..sort_job(&nb.particles, step.out)
                },
                &SORT_LABELS,
            )?;
            self.narrow_full_count = true;
            enc.copy_buffer_to_buffer(out_faces, &nb.previous_faces, out_faces.size);
            self.narrow.initialized = true;
            self.filled = false;
            if let Some(interior) = self.interior.as_ref() {
                enc.copy_buffer_to_buffer(&nb.phi, interior, nb.phi.size);
            }
        } else if let Some(interior) = self.interior.as_ref() {
            enc.dispatch_compute(&pipes.narrow_disabled, &[uniform(&base), buffer(5, interior)], cells_groups, "gpu_flip.step.narrow.disabled");
        }
        // Native removal precedes inflow. Stable cell ranks predate speed
        // removal: an extreme marker still consumes a cell quota place.
        enc.compute_memory_barrier_buffers();
        self.sorter.encode(device, enc, &SortJob {
            count: base.particles, ..sort_job(step.out, sorted)
        }, &SORT_LABELS)?;
        enc.compute_memory_barrier_buffers();
        self.clock.as_ref().expect("live clock prepared")
            .remove_extreme(enc, sorted, base.particles, clock_params);
        let ranges = self.sorter.ranges().ok_or("the cell ranges were not reserved")?;
        enc.compute_memory_barrier_buffers();
        enc.dispatch_compute(&pipes.remove_crowded,
            &[uniform(&base), buffer(46, step.clock_plan), buffer(1, ranges), buffer(38, sorted)],
            cells_groups, "gpu_flip.step.remove_crowded");
        enc.compute_memory_barrier_buffers();
        self.sorter.encode(device, enc, &SortJob {
            count: base.particles, ..sort_job(sorted, step.out)
        }, &SORT_LABELS)?;
        // New inflow is first transferred and advected by the following step.
        if p.region_count > 0 {
            let sites = emit_sites(cells);
            let ranges = self.sorter.ranges().ok_or("the cell ranges were not reserved")?;
            let scan = self.emit_scan.buffer(device, sites as usize)?.clone();
            let emission = StepParams { capacity: base.particles, ..base };
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(&pipes.emit_flags, &[
                uniform(&emission), buffer(1, ranges), buffer(2, step.out),
                buffer(9, &l.corners), buffer(15, step.shapes), buffer(16, step.atlas),
                buffer(36, step.regions), buffer(46, step.clock_plan), buffer(37, &scan),
            ], groups(sites), "gpu_flip.step.emit_flags");
            enc.compute_memory_barrier_buffers();
            self.emit_scan.encode_labelled(enc, sites as usize, ScanLabels {
                blocks: "gpu_flip.step.emit_scan.blocks", add: "gpu_flip.step.emit_scan.add",
            });
            enc.dispatch_compute(&pipes.emit_write, &[
                uniform(&emission), buffer(1, ranges), buffer(15, step.shapes),
                buffer(16, step.atlas), buffer(36, step.regions), buffer(46, step.clock_plan),
                buffer(37, &scan), buffer(38, step.out),
            ], groups(sites), "gpu_flip.step.emit_write");
        }
        enc.compute_memory_barrier_buffers();
        // A failure is sticky across narrow enable toggles and is cleared only
        // by an epoch/lattice reset.  Never-enabled dense steps use the
        // existing zero storage, so they do not acquire a new scratch word.
        let status = self
            .narrow
            .buffers
            .as_ref()
            .map(|buffers| &buffers.failure)
            .unwrap_or(self.zeros.as_ref().expect("zero storage prepared"));
        enc.dispatch_compute(
            &pipes.narrow_tally,
            &[
                uniform(&base),
                GpuBinding::Buffer {
                    binding: 22,
                    buffer: step.capped,
                    offset: step.tally,
                },
                buffer(43, status),
            ],
            [1, 1, 1],
            "gpu_flip.step.narrow.tally",
        );
        Ok(())
    }
}

/// The Solve Level, refused unless it is a whole number the lattice's
/// levels reach (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 11 (Solve Level)):
/// never clamped, so a level the lattice lacks is a named error.
pub(crate) fn read_solve_level(value: f32, lattice: [u32; 3]) -> Result<usize, String> {
    if !(value.is_finite() && value >= 0.0 && value.fract() == 0.0) {
        return Err(format!("Solve Level must be a whole number from 0, not {value}"));
    }
    let level = value as usize;
    match level_refusal(lattice, level) {
        Some(reason) => Err(reason),
        None => Ok(level),
    }
}

/// The Closed Faces mask, refused unless it is a whole number in 0..=63.
pub(crate) fn read_closed_faces(value: f32) -> Result<u32, String> {
    if value.fract() == 0.0 && (0.0..=63.0).contains(&value) {
        Ok(value as u32)
    } else {
        Err(format!("Closed Faces must be a whole number from 0 to 63, not {value}"))
    }
}

/// Narrow-band is a strict binary step option.  Invalid values are refused
/// so a malformed wire cannot silently select a different numerical method.
pub(crate) fn read_narrow_band(value: f32) -> Result<bool, String> {
    match value {
        0.0 => Ok(false),
        1.0 => Ok(true),
        _ => Err(format!("Narrow Band must be exactly 0 or 1, not {value}")),
    }
}

fn read_epoch(value: f32) -> Result<u32, String> {
    if value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= u32::MAX as f32 {
        Ok(value as u32)
    } else {
        Err(format!("Epoch must be a finite nonnegative whole number, not {value}"))
    }
}

crate::primitive! {
    name: GpuFlipStep,
    type_id: "node.gpu_flip_step",
    purpose: "Advance GPU FLIP water through the accepted clock interval with CFL 5 and Steps minimum substeps (1 by default); each substep sorts the particles into the lattice's cells, gathers their velocity onto the cell faces, adds gravity and the scene's forces and impulses, makes the water incompressible against the tank walls and the scene's solid bodies (a multigrid-preconditioned pressure solve, the free surface placed where the particles' distance crosses zero), moves crowded particles apart and sparse ones together so the water keeps its volume (a density projection, position only, when Volume Projection is 1), then moves every particle through the new velocity, blending FLIP and PIC by Flip Share, and keeps it out of the solid bodies (a particle a moving body swept over is removed). A face Closed Faces leaves open (bit 2d the low face of axis d, bit 2d + 1 the high one) drains: every wall stays solid, and a particle that ends a substep within 2 cells of an open face is removed. Water a moving solid seals off from air (and from any open face) does not take that solid's push, unless the bodies are in the solve.When dynamic_bodies is above 0, each body that takes a reaction joins the pressure solve with its own velocity, so the water pushes it and it pushes back in the same solve, and the step adds the pressure's and the friction's impulse on every body to the reaction. Extrapolates projected velocity by 12 layers before constraining solids. Removes markers above 250 per cell and extreme velocities using the accepted interval, compacts survivors without changing their ids, then emits inflow for the next step. Outputs the moved particles, the step's face grid (valid at least 2 layers around the water) and the reaction, in place.",
    inputs: {
        particles: Array(FluidParticle) required,
        count: ScalarF32 optional,
        lattice_min_x: ScalarF32 optional, lattice_min_y: ScalarF32 optional, lattice_min_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity_y: ScalarF32 optional, gravity_z: ScalarF32 optional,
        forces: Array(f32) optional,
        impulses: Array(f32) optional,
        field_nodes_x: ScalarF32 optional, field_nodes_y: ScalarF32 optional, field_nodes_z: ScalarF32 optional,
        field_spacing: ScalarF32 optional,
        force_lattices: ScalarF32 optional,
        impulse_tick: ScalarF32 optional,
        first_tick: ScalarF32 optional,
        tick_index: ScalarF32 optional,
        interval_duration: ScalarF32 optional, bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        body_count: ScalarF32 optional,
        regions: Array(LiquidBody) optional,
        region_count: ScalarF32 optional,
        rows: ScalarF32 optional,
        reaction: Array(f32) optional,
        dynamic_bodies: ScalarF32 optional,
        closed_faces: ScalarF32 optional,
        solve_level: ScalarF32 optional,
        clock_obstacles: Array(f32) optional,
        clock_obstacle_count: ScalarF32 optional,
        clock_sources: Array(f32) optional,
        clock_source_count: ScalarF32 optional,
        live_hits: Array(f32) optional,
        live_hit_count: ScalarF32 optional,

        narrow_band: ScalarF32 optional,
        epoch: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
        faces: Array(FaceSample),
        distance: Array(f32),
        substep_schedule: Array(f32),
        substep_u: Array(f32), substep_v: Array(f32), substep_w: Array(f32),
        substep_count: ScalarF32,
        grid_bounds: Transform,
        grid_nodes_x: ScalarF32, grid_nodes_y: ScalarF32, grid_nodes_z: ScalarF32,
        face_cells_x: ScalarF32, face_cells_y: ScalarF32, face_cells_z: ScalarF32,
        face_valid_layers: ScalarF32,
        interior: Array(f32),
        reaction_out: Array(f32),
        capped: Array(u32),
        clock_status: Array(u32),
    },
    params: [
        float_param!("lattice_min_x", "Lattice Min X", -2.1875, -1.0e4, 1.0e4),
        float_param!("lattice_min_y", "Lattice Min Y", -0.1875, -1.0e4, 1.0e4),
        float_param!("lattice_min_z", "Lattice Min Z", -2.1875, -1.0e4, 1.0e4),
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        int_param!("nodes_x", "Nodes X", 71.0, 8.0, 1024.0),
        int_param!("nodes_y", "Nodes Y", 71.0, 8.0, 1024.0),
        int_param!("nodes_z", "Nodes Z", 71.0, 8.0, 1024.0),
        float_param!("gravity_x", "Gravity X", 0.0, -100.0, 100.0),
        float_param!("gravity_y", "Gravity Y", -9.81, -100.0, 100.0),
        float_param!("gravity_z", "Gravity Z", 0.0, -100.0, 100.0),
        int_param!("field_nodes_x", "Field Nodes X", 2.0, 2.0, 4096.0),
        int_param!("field_nodes_y", "Field Nodes Y", 2.0, 2.0, 4096.0),
        int_param!("field_nodes_z", "Field Nodes Z", 2.0, 2.0, 4096.0),
        float_param!("field_spacing", "Field Spacing", 0.25, 1.0e-4, 400.0),
        int_param!("force_lattices", "Force Lattices", 0.0, 0.0, 16_777_216.0),
        int_param!("impulse_tick", "Impulse Tick", -1.0, -1.0, 16_777_216.0),
        int_param!("first_tick", "First Tick", 0.0, 0.0, 16_777_216.0),
        int_param!("tick_index", "Tick", 0.0, 0.0, 16_777_216.0),
        int_param!("body_count", "Bodies", 0.0, 0.0, MAX_FLUID_ROLES as f32),
        int_param!("rows", "Rows", 0.0, 0.0, 16_777_216.0),
        int_param!("region_count", "Regions", 0.0, 0.0, MAX_FLUID_ROLES as f32),
        float_param!("inflow_jitter", "Inflow Jitter", 0.0, 0.0, 1.0),
        int_param!("steps", "Steps", 1.0, 1.0, 64.0),
        float_param!("flip", "Flip Share", 0.95, 0.0, 1.0),
        int_param!("iterations", "Iterations (0 = Auto)", 0.0, 0.0, MAX_ITERATIONS as f32),
        float_param!("top_speed", "Top Speed", DEFAULT_TOP_SPEED, 0.1, 1000.0),
        int_param!("ghost_fluid", "Ghost Fluid", 1.0, 0.0, 1.0),
        int_param!("volume_projection", "Volume Projection", 1.0, 0.0, 1.0),
        int_param!("closed_faces", "Closed Faces", 63.0, 0.0, 63.0),
        int_param!("solve_level", "Solve Level", 0.0, 0.0, 4.0),
        int_param!("narrow_band", "Narrow Band", 0.0, 0.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Inside node.liquid_state's tick region, once per tick: particles from the state's out, Steps substeps of 1/(60·Steps) s run inside the node, each moving the last one's particles, and the bodies see every substep. The lattice, gravity, the field scalars, forces, impulses, bodies, shapes, atlas, body_count and body_rows (into rows) come from node.gpu_flip_domain; so do dynamic_bodies and reaction, which every substep adds to in place; tick_index from node.liquid_state; count from the fill's live count. Flip Share is the share kept per 1/60 s, so the damping does not change with the step count. The last substep's faces feed node.liquid_state's faces_in, sized exactly to the lattice; out keeps the particles slots. regions and region_count also come from node.gpu_flip_domain: each step an inflow seeds particles at its empty half-cell sites into free pool slots (a full pool emits nothing) and holds the velocity inside it, and an outflow kills the particles it holds; the live count rides the cell ranges. A lattice the device cannot hold, or a side over 1024 cells, is a named error.",
    examples: ["WaterDamBreakGpuFlip"],
    picker: { label: "GPU FLIP Step", category: Atom },
    summary: "Moves the water forward one tick, in Steps substeps: gravity, solids, incompressibility and the particles' motion.",
    category: Particles3D,
    role: Filter,
    aliases: ["flip step", "water step", "pressure solve", "fluid solver"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        state: StepState = StepState::default(),
    },
}

/// When the solves stop: Auto at 0 or below converges within the cap, a
/// value runs exactly that many iterations, refused past MAX_ITERATIONS.
fn read_iterations(value: f32) -> Result<Stop, String> {
    match value.round() {
        v if v <= 0.0 => Ok(Stop::Converged(AUTO_PRESSURE_ITERATIONS)),
        v if v > MAX_ITERATIONS as f32 => Err(format!("Iterations {v} is past the solver's {MAX_ITERATIONS}")),
        v => Ok(Stop::Fixed(v as u32)),
    }
}

impl Primitive for GpuFlipStep {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.state.prepare_pipelines(device);
        self.state.history.prepare(device);
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "faces" | "distance" | "interior" | "substep_schedule" | "substep_u" | "substep_v" | "substep_w")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "faces" => self.state.faces.as_ref(),
            "distance" => self.state.lattice.as_ref().map(|l| &l.phi),
            "interior" => self.state.interior.as_ref(),
            "substep_schedule" => self.state.history.schedule.as_ref(),
            "substep_u" | "substep_v" | "substep_w" => self.state.history.faces.as_ref().map(|f| &f[match port { "substep_u" => 0, "substep_v" => 1, _ => 2 }]),
            _ => None,
        }
    }

    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        match port {
            "out" => inputs.iter().find(|(name, _)| *name == "particles").map(|&(_, n)| n),
            // Provided storage: a one-record hint, sized to the lattice at run time.
            "faces" | "distance" | "interior" | "substep_schedule" | "substep_u" | "substep_v" | "substep_w" => Some(1),
            "reaction_out" => inputs.iter().find(|(name, _)| *name == "reaction").map(|&(_, n)| n),
            "capped" => inputs.iter().find(|(name, _)| *name == "particles").map(|&(_, n)| n.saturating_mul(2).saturating_add(SOLVER_WORDS)),
            "clock_status" => Some(8),
            _ => None,
        }
    }

    fn clear_state(&mut self) {
        self.state.narrow_history = None;
        self.state.narrow.initialized = false;
        self.state.narrow_reset_pending = true;
        self.state.narrow_full_count = false;
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("reaction", "reaction_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(lattice) = LiquidLattice::from_wires(ctx, NAME) else {
            return;
        };
        let cells = lattice.cells();
        ctx.outputs.set_transform("grid_bounds", lattice.bounds());
        for (port, value) in ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"].into_iter().zip(lattice.nodes())
            .chain(["face_cells_x", "face_cells_y", "face_cells_z"].into_iter().zip(cells))
            .chain([("face_valid_layers", FACE_VALID_LAYERS)]) {
            ctx.outputs.set_scalar(port, ParamValue::Float(value as f32));
        }
        if let Some(reason) = lattice_refusal(cells) {
            ctx.error(format!("{NAME}: {reason}. Lower Resolution."));
            return;
        }
        let field = match FieldBinding::read(ctx, ctx.inputs.array("forces"), ctx.inputs.array("impulses"), NAME) {
            Ok(field) => field,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let (Some(particles), Some(out), Some(capped)) = (ctx.inputs.array("particles"), ctx.outputs.array("out"), ctx.outputs.array("capped")) else {
            return;
        };
        let particle_bytes = size_of::<FluidParticle>() as u64;
        let Ok(capacity) = u32::try_from(particles.size / particle_bytes) else {
            ctx.error(format!("{NAME}: particle storage exceeds 32-bit slot addressing"));
            return;
        };
        let out_slots = (out.size / particle_bytes).min(u64::from(capacity)) as u32;
        let narrow_enabled = match read_narrow_band(ctx.scalar_or_param("narrow_band", 0.0)) {
            Ok(enabled) => enabled,
            Err(error) => {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
        };
        if narrow_enabled && out_slots != capacity {
            ctx.error(format!("{NAME}: Narrow Band requires out to hold all {capacity} particle slots"));
            return;
        }
        let tally = 2 * u64::from(capacity) * 4;
        if capped.size < tally + u64::from(SOLVER_WORDS) * 4 {
            ctx.error(format!("{NAME}: the capped array holds fewer than two words a particle slot and {SOLVER_WORDS} solver words"));
            return;
        }
        let count = match ctx.inputs.scalar("count") {
            Some(ParamValue::Float(count)) if count.is_finite() => {(count.max(0.0) as u32).min(capacity)},
            _ => capacity,
        };
        let steps = ctx.scalar_or_param("steps", 1.0).round().clamp(1.0, 64.0);
        let interval_duration = f64::from(ctx.scalar_or_param("interval_duration", TICK as f32));
        let step_dt = (interval_duration/ f64::from(steps)) as f32;
        let flip = ctx.scalar_or_param("flip", 0.95).clamp(0.0, 1.0);
        let top_speed = ctx.scalar_or_param("top_speed", DEFAULT_TOP_SPEED);
        if !(top_speed.is_finite() && top_speed > 0.0) {
            ctx.error(format!("{NAME}: Top Speed must be positive, not {top_speed}"));
            return;
        }
        let h = lattice.cell_size();
        let travel = travel_cells(top_speed, step_dt, h);
        let gravity = [("gravity_x", 0.0), ("gravity_y", -9.81), ("gravity_z", 0.0)].map(|(name, default)| ctx.scalar_or_param(name, default));
        let tick_index = ctx.scalar_or_param("tick_index", 0.0).round().max(0.0) as i32;
        let epoch = if ctx.inputs.slot("epoch").is_some() {
            match read_epoch(ctx.scalar_or_param("epoch", 0.0)) {
                Ok(epoch) => Some(epoch),
                Err(error) => {
                    ctx.error(format!("{NAME}: {error}"));
                    return;
                }
            }
        } else {
            None
        };
        let body_count = ctx.scalar_or_param("body_count", 0.0).round().clamp(0.0, MAX_FLUID_ROLES as f32) as i32;
        let rows = ctx.scalar_or_param("rows", 0.0).round().max(0.0) as i32;
        let pressure = match read_iterations(ctx.scalar_or_param("iterations", 0.0)) {
            Ok(iterations) => iterations,
            Err(error) => {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
        };
        let ghost = ctx.scalar_or_param("ghost_fluid", 1.0) > 0.5;
        let closed_faces = match read_closed_faces(ctx.scalar_or_param("closed_faces", 63.0)) {
            Ok(mask) => mask,
            Err(error) => {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
        };
        let level = match read_solve_level(ctx.scalar_or_param("solve_level", 0.0), cells) {
            Ok(level) => level,
            Err(error) => {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
        };
        let box_min = lattice.box_min();
        let regions_in = ctx.inputs.array("regions");
        let region_count = match regions_in {
            Some(_) => ctx.scalar_or_param("region_count", 0.0).round().clamp(0.0, MAX_FLUID_ROLES as f32) as i32,
            None => 0,
        };
        let band = band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS);
        let restore_narrow = self.state.sync_narrow_history(epoch, box_min, h, cells, capacity, tick_index, narrow_enabled);
        if (narrow_enabled || restore_narrow || self.state.narrow_full_count) && out_slots != capacity {
            ctx.error(format!("{NAME}: Narrow Band history requires out to hold all {capacity} particle slots"));
            return;
        }
        if (narrow_enabled || restore_narrow || self.state.narrow_full_count) && capacity > EXACT_F32_COUNT {
            ctx.error(format!("{NAME}: Narrow Band requires at most {EXACT_F32_COUNT} particle slots for exact count addressing"));
            return;
        }
        let interior_wired = ctx.outputs.slot("interior").is_some();
        if let Err(error) = self
            .state
            .reserve(ctx.gpu_encoder().device, cells, u64::from(capacity), ring_max(band), region_count > 0, narrow_enabled, interior_wired)
        {
            ctx.error(format!(
                "{NAME}: a {}×{}×{} lattice with {capacity} particle slots needs storage the device cannot give: {error}. Lower Resolution.",
                cells[0], cells[1], cells[2]
            ));
            return;
        }
        let zeros = self.state.zeros.clone().expect("zeros prepared");
        let clock_obstacles = ctx.inputs.array("clock_obstacles").unwrap_or(&zeros);
        let clock_sources = ctx.inputs.array("clock_sources").unwrap_or(&zeros);
        let obstacle_count = ctx
            .scalar_or_param("clock_obstacle_count", 0.0)
            .round()
            .max(0.0)
            .min(
                (clock_obstacles.size
                    / size_of::<super::gpu_flip_clock::GpuFlipBodyVertex>() as u64)
                    as f32,
            ) as u32;
        let source_count = ctx
            .scalar_or_param("clock_source_count", 0.0)
            .round()
            .max(0.0)
            .min(
                (clock_sources.size / size_of::<super::gpu_flip_clock::GpuFlipBodyVertex>() as u64)
                    as f32,
            ) as u32;
        let live_hits = ctx.inputs.array("live_hits").unwrap_or(&zeros);
        let live_hit_count = ctx
            .scalar_or_param("live_hit_count", 0.0)
            .round()
            .max(0.0)
            .min((live_hits.size / 16) as f32) as u32;
        {
            let capacities = [capacity, obstacle_count.max(1), source_count.max(1)];
            if self.state.clock.is_none()
                || self
                    .state
                    .clock_capacities
                    .iter()
                    .zip(capacities)
                    .any(|(old, new)| *old < new)
            {
                self.state.clock = Some(GpuFlipClock::new(
                    ctx.gpu_encoder().device,
                    capacities[0],
                    capacities[1],
                    capacities[2],
                ));
                self.state.clock_capacities = capacities;
            }
        }
        let bodies_in = ctx.inputs.array("bodies");
        let shapes_in = ctx.inputs.array("shapes");
        let atlas_in = ctx.inputs.array("atlas");
        let (bodies, shapes, atlas, body_count, rows, shapes_len) = match (bodies_in, shapes_in, atlas_in) {
            (Some(bodies), Some(shapes), Some(atlas)) => {
                let held = (bodies.size / size_of::<LiquidBody>() as u64).min(i32::MAX as u64) as i32;
                let shapes_len = (shapes.size / size_of::<LiquidShape>() as u64).min(u64::from(u32::MAX)) as u32;
                // Rows are tick major: this tick's are the last of the prefix
                // the solid passes read.
                let through_tick = (tick_index - field.first_tick + 1).max(1).saturating_mul(body_count);
                (bodies, shapes, atlas, body_count, rows.min(held).min(through_tick), shapes_len)
            }
            _ => (&zeros, &zeros, &zeros, 0, 0, 0),
        };
        let reaction_in = ctx.inputs.array("reaction");
        let dynamic = body_count > 0 && ctx.scalar_or_param("dynamic_bodies", 0.0) > 0.0;
        if dynamic && let Some(reason) = body_refusal(body_count as u32, reaction_in) {
            ctx.error(format!("{NAME}: {reason}"));
            return;
        }
        let reaction = reaction_in.unwrap_or(&zeros);
        let regions = regions_in.unwrap_or(&zeros);
        // The engine refuses a negative jitter (setMarkerParticleJitterFactor).
        let inflow_jitter = ctx.scalar_or_param("inflow_jitter", 0.0);
        if !(inflow_jitter.is_finite() && inflow_jitter >= 0.0) {
            ctx.error(format!("{NAME}: Inflow Jitter must be 0 or more, not {inflow_jitter}"));
            return;
        }
        let region_rows = (regions.size / size_of::<LiquidBody>() as u64).min(i32::MAX as u64) as i32;
        let mut step = Step {
            params: StepParams {
                n: cells,
                capacity,
                box_min,
                cell_size: h,
                gravity,
                step_dt,
                field_nodes: field.nodes.map(|n| n as u32),
                field_spacing: field.spacing,
                tick_index,
                step_in_tick: 0,
                force_lattices: field.force_lattices,
                impulse_tick: field.impulse_tick,
                first_tick: field.first_tick,
                body_count,
                rows,
                tick_seconds: step_dt,
                // The share is per step, as the engine's `_ratioPICFLIP`, whatever
                // the step count.
                flip,
                max_travel: travel as f32,
                box_offset: box_min.iter().fold(0.0_f32, |m, v| m.max(v.abs())),
                ghost: u32::from(ghost),
                particles: out_slots,
                shapes_len,
                // The whole density error each step: projected to rest, no
                // per-step share.
                rate: 1.0 / step_dt,
                closed_faces,
                region_count,
                region_rows,
                emit_jitter: 0.25 * inflow_jitter,
                solve_level: level as u32,
                all_tiles: u32::from(all_tiles()),
                ring_cap: 0,
                ring_max: ring_max(band),
                live_impulse_stride: field
                    .nodes
                    .iter()
                    .map(|&n| n as usize)
                    .product::<usize>()
                    .saturating_mul(4)
                    .min(u32::MAX as usize) as u32,
                narrow_band: u32::from(narrow_enabled),
                clock_pad: [0; 3],
            },
            clock_plan: &zeros,
            particles,
            out,
            capped,
            count: if restore_narrow || self.state.narrow_full_count { capacity } else { count },
            forces: field.forces.unwrap_or(&zeros),
            impulses: field.impulses.unwrap_or(&zeros),
            bodies,
            regions,
            shapes,
            atlas,
            reaction,
            dynamic,
            pressure,
            level,
            tally,
            band,
            ghost,
            density: ctx.scalar_or_param("volume_projection", 1.0) > 0.5,
            narrow_enabled,
            restore_narrow,
        };
        let  clock_status = ctx.outputs.array("clock_status").cloned();
        let body_row_offset = u64::try_from(rows.saturating_sub(body_count).max(0))
            .unwrap_or(0)
            .saturating_mul(size_of::<LiquidBody>() as u64);
        let history_slots = manifold_physics::stepping::LIVE_DEFAULT_MAX_STEPS + live_hit_count;
        ctx.outputs.set_scalar("substep_count", ParamValue::Float(history_slots as f32));
        let gpu = ctx.gpu_encoder();
        self.state.history.prepare(gpu.device);
        if let Err(error) = self.state.history.reserve(gpu.device, step.params.n, history_slots) {
            ctx.error(format!("{NAME}: {error}"));
            return;
        }
        // The first substep reads the tick's particles, every later one the
        // last one's out. The reaction is in place, so the bodies feel every
        // substep.
        let clock_params = GpuFlipClockParams {
            frame_duration: interval_duration as f32,
            cell_size: h,
            cfl: ENGINE_CFL as f32,
            surface_condition: 1.0,
            surface_constant: 1.0,
            color_mixing_rate: 0.0,
            _pad_prediction: 0.0,
            _pad0: 0.0,
            min_frame_steps: steps as u32,
            max_frame_steps: manifold_physics::stepping::LIVE_DEFAULT_MAX_STEPS,
            flags: if tick_index == 0 {
                clock_flags::FIRST_SUBSTEP | clock_flags::FLUID_PRESENT_OR_GENERATING
            } else {
                clock_flags::FLUID_PRESENT_OR_GENERATING
            },
            interval_sequence: tick_index as u32,
            constant_force: [gravity[0], gravity[1], gravity[2], 0.0],
        };
        let mut last_clock_plan = zeros.clone();
        let encoded_steps = manifold_physics::stepping::LIVE_DEFAULT_MAX_STEPS as f32 + live_hit_count as f32;
        {
            let clock = self.state.clock.as_ref().expect("live clock prepared");
            clock.begin_frame(gpu.native_enc, &clock_params);
        }
        for k in 0..encoded_steps as i32 {
            step.restore_narrow = restore_narrow && k == 0;
            step.count = if k > 0 { out_slots } else if self.state.narrow_full_count || step.restore_narrow { capacity } else { count };
            let particles_for_step = if k > 0 { out } else { particles };
            let params = StepParams {
                step_in_tick: k,
                tick_seconds: (k + 1) as f32 * step_dt,
                ..step.params
            };
            {
                let plan_buffer = {
                    let clock = self.state.clock.as_ref().expect("live clock prepared");
                    let plan = clock.dispatch(
                        gpu.native_enc,
                        GpuFlipClockInputs {
                            marker_particles: particles_for_step,
                            marker_count:step.count,
                            obstacle_vertices: clock_obstacles,
                            obstacle_count,
                            source_vertices: clock_sources,
                            source_count,
                            live_hits,
                            live_hit_count,
                            event_impulses: field.impulses.unwrap_or(&zeros),
                            impulse_stride: step.params.live_impulse_stride,
                            impulse_nodes: step.params.field_nodes,
                            impulse_origin: step.params.box_min,
                            impulse_spacing: step.params.field_spacing,
                            body_rows: bodies,
                            body_rows_offset: body_row_offset,
                            body_reaction: reaction,
                        },
                        &clock_params,
                    );
                    plan.buffer().clone()
                };
                let iteration = Step {
                    params,
                    particles: particles_for_step,
                    clock_plan: &plan_buffer,
                    ..step
                };
            if let Err(error) = self.state.encode(gpu.device, gpu.native_enc, &iteration, &clock_params) {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
                self.state.commit_mask(
                    gpu.native_enc,
                    &plan_buffer,
                    out,
                    self.state
                        .saved_particles
                        .as_ref()
                        .expect("particle mask prepared"),
                    "gpu_flip.step.mask.particles",
                );
                self.state.commit_mask(
                    gpu.native_enc,
                    &plan_buffer,
                    self.state.faces.as_ref().expect("face mask prepared"),
                    self.state.saved_faces.as_ref().expect("face mask prepared"),
                    "gpu_flip.step.mask.faces",
                );
                self.state.commit_mask(
                    gpu.native_enc,
                    &plan_buffer,
                    &self.state.lattice.as_ref().expect("lattice prepared").phi,
                    self.state
                        .saved_distance
                        .as_ref()
                        .expect("distance mask prepared"),
                    "gpu_flip.step.mask.distance",
                );
                self.state.commit_mask(
                    gpu.native_enc,
                    &plan_buffer,
                    step.reaction,
                    self.state
                        .saved_reaction
                        .as_ref()
                        .expect("reaction mask prepared"),
                    "gpu_flip.step.mask.reaction",
                );
                self.state.commit_mask(
                    gpu.native_enc,
                    &plan_buffer,
                    step.capped,
                    self.state
                        .saved_capped
                        .as_ref()
                        .expect("capped mask prepared"),
                    "gpu_flip.step.mask.capped",
                );
                if step.narrow_enabled {
                    let nb = self.state.narrow.buffers.as_ref().expect("narrow buffers prepared");
                    for (target, saved) in [
                        (&nb.phi, self.state.saved_narrow_phi.as_ref()),
                        (&nb.previous_faces, self.state.saved_narrow_faces.as_ref()),
                        (&nb.failure, self.state.saved_narrow_failure.as_ref()),
                    ] {
                        self.state.commit_mask(gpu.native_enc, &plan_buffer, target,
                            saved.expect("narrow mask prepared"), "gpu_flip.step.mask.narrow");
                    }
                    if let Some(interior) = &self.state.interior {
                        gpu.native_enc.copy_buffer_to_buffer(&nb.phi, interior, nb.phi.size);
                    }
                }
                self.state.history.capture(gpu.native_enc, k as u32, &plan_buffer, self.state.faces.as_ref().expect("faces prepared"));
                last_clock_plan = plan_buffer;
            }
        }
        if let Some(status) = clock_status {
            gpu.native_enc
                .copy_buffer_to_buffer(&last_clock_plan, &status, 32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_band_history_restores_once_and_resets_identity() {
        let mut state = StepState::default();
        let mut sync = |epoch, tick, enabled| state.sync_narrow_history(epoch, [0.0; 3], 1.0, [8; 3], 4096, tick, enabled);
        assert!(!sync(Some(1), 0, true));
        assert!(!sync(Some(1), 1, true));
        assert!(sync(Some(1), 2, false));
        assert!(!sync(Some(1), 3, false));
        assert!(!sync(Some(1), 4, true));
        assert!(!sync(Some(2), 0, false));
        assert!(!sync(None, 10, true));
        assert!(!sync(None, 0, false));
        assert!(state.narrow_reset_pending);
        assert!(!state.narrow_full_count);
        assert!(!state.narrow.initialized);
    }

    #[test]
    fn narrow_band_mode_refuses_fractional_and_nonfinite_values() {
        assert_eq!(read_narrow_band(0.0), Ok(false));
        assert_eq!(read_narrow_band(1.0), Ok(true));
        for value in [-1.0, 0.5, 2.0, f32::NAN, f32::INFINITY] {
            assert!(read_narrow_band(value).is_err());
        }
    }

    /// I8: the liquid conformance suite checks codegen bodies for atomics;
    /// the step's hand shader is checked here. Its only atomics are the
    /// pockets' fixed-point sums.
    #[test]
    fn step_shader_atomics_are_allowlisted_integer_sums() {
        let stray = super::atomic_sites_outside(STEP_SHADER, POCKET_ATOMIC_SITES);
        assert!(stray.is_empty(), "atomics outside the allowlist: {stray:#?}");
    }

    /// The pocket sums' two-word add with carry (gpu_flip_step.wgsl
    /// pocket_fixed, pocket_add, pocket_value) gives the exact sum of the
    /// fixed-point values in any order.
    #[test]
    fn pocket_carry_sum_is_exact_in_any_order() {
        const SCALE: f64 = 65536.0;
        let fixed = |x: f64| -> (u32, u32) {
            let v = (x * SCALE).round() as i64;
            (v as u32, (v >> 32) as u32)
        };
        let add = |words: &mut (u32, u32), q: (u32, u32)| {
            let (low, carry) = words.0.overflowing_add(q.0);
            words.0 = low;
            words.1 = words.1.wrapping_add(q.1).wrapping_add(u32::from(carry));
        };
        let value = |w: (u32, u32)| ((i64::from(w.1 as i32) << 32) | i64::from(w.0)) as f64 / SCALE;
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        // Mixed signs and magnitudes, some past 2^32 in fixed point.
        let values: Vec<f64> = (0..20_000)
            .map(|i| {
                let unit = (next() % 2_000_001) as f64 / 1.0e6 - 1.0;
                unit * if i % 7 == 0 { 1.0e5 } else { 3.0 }
            })
            .collect();
        let want: f64 = values.iter().map(|&x| (x * SCALE).round()).sum::<f64>() / SCALE;
        let mut order: Vec<usize> = (0..values.len()).collect();
        let mut totals = Vec::new();
        for _ in 0..3 {
            for i in (1..order.len()).rev() {
                order.swap(i, (next() % (i as u64 + 1)) as usize);
            }
            let mut words = (0u32, 0u32);
            for &i in &order {
                add(&mut words, fixed(values[i]));
            }
            totals.push(words);
        }
        assert!(totals.windows(2).all(|w| w[0] == w[1]), "the words differ by order: {totals:?}");
        assert_eq!(value(totals[0]), want, "the carry sum equals the exact sum");
    }

    #[test]
    fn step_shader_validates_with_every_entry() {
        let mask = naga::front::wgsl::parse_str(MASK_SHADER)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(MASK_SHADER)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&mask)
        .unwrap_or_else(|e| panic!("{e:?}"));        for source in [step_source(), band_source()] {
            let module = naga::front::wgsl::parse_str(&source).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
                .validate(&module)
                .unwrap_or_else(|e| panic!("{e:?}"));
            let mut bindings = std::collections::HashSet::new();
            for (_, global) in module.global_variables.iter() {
                if let Some(binding) = &global.binding {
                    assert!(bindings.insert((binding.group, binding.binding)), "duplicate shader binding {binding:?}");
                }
            }
            let entries: Vec<&str> = module.entry_points.iter().map(|e| e.name.as_str()).collect();
            for entry in [
                "particles_to_faces",
                "extend_faces",
                "face_gravity",
                "open_fractions",
                "solid_face_velocity",
                "phi_into_solids",
                "water_from_phi",
                "divergence",
                "particle_distance",
                "subtract_pressure",
                "constrain_solid_faces",
                "remove_crowded_markers",
                "density_source",
                "faces_to_particles",
                "tiles_classify",
                "tiles_rings",
                "tiles_lists",
                "tiles_fill",
                "tiles_retire",
                "separate_pin",
                "separate_update",
                "pocket_leader_clear",
                "pocket_coarsen",
                "pocket_relabel",
            ] {
                assert!(entries.contains(&entry), "missing entry {entry}");
            }
        }
    }

    /// Separating solids' extents: the lattice allocates exactly
    /// [`LATTICE_CELL_ARRAYS`] cell-sized arrays, which the extent's hold
    /// counts; the two new passes run one thread a cell, bounded by the cell
    /// total, and bind the contact mask and the let-go set at their own
    /// bindings.
    #[test]
    fn gpu_flip_separating_solids_buffers_and_dispatches_are_held() {
        let source = include_str!("gpu_flip_step.rs");
        let body = source.split("fn new(device: &GpuDevice, cells: [u32; 3]) -> Result<Self, String> {\n        let cell").nth(1).expect("LatticeBuffers::new");
        let body = body.split("\n    }\n").next().unwrap_or("");
        let arrays = body.matches("(device, cell)?").count() as u64;
        // The fine-label leader is counted by pocket_coarse_bytes, separately
        // from the nine fine solve arrays.
        assert_eq!(arrays, LATTICE_CELL_ARRAYS + 1, "cell arrays including the coarse pocket leader");
        for cells in [[64u32, 64, 64], [63, 100, 8], [128, 128, 128]] {
            let face = face_bytes(cells);
            let corners = cells.iter().map(|&n| u64::from(n) + 1).product::<u64>() * 4;
            let particles = 1000 * size_of::<FluidParticle>() as u64;
            let expected = particles
                + 9 * cell_bytes(cells)
                + corners
                + 6 * face
                + POCKET_GATE_WORDS * 4
                + pocket_sum_bytes(cells)
                + pocket_coarse_bytes(cells)
                + tile_scratch_bytes(cells, 2)
                + mask_saved_bytes(cells, 1000);
            assert_eq!(scratch_bytes(cells, 1000, 2), expected, "{cells:?}");
        }
        let shader = step_source();
        for entry in ["fn separate_pin(", "fn separate_update("] {
            let body = shader.split(entry).nth(1).unwrap_or_else(|| panic!("{entry}"));
            let head: String = body.lines().take(4).collect();
            assert!(head.contains("if idx >= cell_total()"), "{entry} bounds its threads by the cell total");
        }
        assert!(shader.contains("@binding(42) var<storage, read_write> let_go: array<f32>;"));
        assert!(source.contains("buffer(42, &l.let_go), buffer(5, &l.contact_water)"));
    }

    #[test]
    fn step_params_match_the_shader_uniform() {
        assert_eq!(size_of::<StepParams>(), 176);
    }

    /// The tile table's bytes, by an independent count: five words a tile
    /// (nearness, two ring halves, the list, the retired list), the counts
    /// and the triples; partial edge tiles counted whole; the extent's hold
    /// grows by exactly that.
    #[test]
    fn gpu_flip_tile_table_bytes_follow_the_lattice() {
        assert_eq!(tile_counts([64, 64, 64]), [8, 8, 8]);
        assert_eq!(tile_counts([63, 100, 8]), [8, 13, 1]);
        assert_eq!(tile_counts([1, 9, 17]), [1, 2, 3]);
        assert_eq!(ring_max(14), 2, "64³: 14 layers reach the second ring");
        assert_eq!(ring_max(23), 3, "128³: 23 layers reach the third");
        assert_eq!(ring_max(FACE_VALID_LAYERS), 1);
        for (cells, tiles) in [([64, 64, 64], 512u64), ([63, 100, 8], 104), ([128, 128, 128], 4096), ([1, 9, 17], 6)] {
            for r in 1..5u32 {
                let counts = u64::from(r) + 2 + 1 + 1;
                let triples = 3 * (u64::from(r) + 1 + 1);
                assert_eq!(tile_scratch_bytes(cells, r), (5 * tiles + counts + triples) * 4, "{cells:?} ring_max {r}");
                assert_eq!(
                    scratch_bytes(cells, 1000, r) - tile_scratch_bytes(cells, r),
                    scratch_bytes(cells, 1000, 1) - tile_scratch_bytes(cells, 1)
                );
            }
        }
    }

    #[test]
    fn closed_faces_takes_the_six_bit_mask_and_refuses_the_rest() {
        assert_eq!(read_closed_faces(0.0), Ok(0));
        assert_eq!(read_closed_faces(63.0), Ok(63));
        assert_eq!(read_closed_faces(61.0), Ok(61));
        for bad in [64.0, -1.0, 2.5, f32::NAN, f32::INFINITY] {
            assert!(read_closed_faces(bad).unwrap_err().contains("Closed Faces"), "{bad}");
        }
    }

    /// The band is the engine's ⌈√3 · CFL⌉ + 3, never under the
    /// face grid's guarantee.
    #[test]
    fn band_layers_match_configured_engine_cfl() {
        assert_eq!(travel_cells(DEFAULT_TOP_SPEED, 1.0 / 120.0, 0.0625), 3);
        assert_eq!(band_layers(3), 9);
        assert_eq!(band_layers(1), 5);
        assert_eq!(band_layers(5), 12, "the engine's 12 layers at its CFL 5");
        assert_eq!(band_layers(8), 17);
        assert_eq!(band_layers(0), 3);
        assert_eq!(band_layers(ENGINE_CFL), 12);
    }
}
