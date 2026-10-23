//! `node.gpu_flip_step` — one GPU FLIP water step (docs/GPU_FLIP_PRESSURE_SOLVE.md
//! section 1 (the step)): sort, particle distance, particles to faces,
//! extend, forces, solids, water mask from φ, divergence, pressure solve,
//! projection, extend, constraint, density projection, then the particles
//! move, remove crowded/extreme markers, compact, and emit inflow. One node
//! because no pass has a consumer outside the step and the solve between them
//! is a barriered reduction; the hand kernels live in
//! `shaders/gpu_flip_step/` (one file per concern, joined in [`STEP_SHADER`]'s
//! order), the solver in [`super::gpu_flip_pressure`].
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
//! particles down its gradient. It does not directly update velocity, but
//! changes later transfers and potential energy. Its cost and effect on
//! native reference agreement remain under audit. Kernel, solid-neighbour weight, surface clamp and
//! ±½ source clamp follow the paper as built in the MIT-licensed `blub`
//! (Copyright (c) 2020 Andreas Reich, github.com/Wumpf/blub,
//! `density_projection_gather_error.comp`; see THIRD_PARTY_NOTICES.md).

use manifold_water_liquid::grid::face_bytes;
use manifold_water_liquid::primitives::prefix_scan::PrefixScan;
use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use super::gpu_flip_bodies::{Bodies, BodyPasses, REACTION_FLOATS, body_refusal};
use super::gpu_flip_clock::{
    GpuFlipClock, GpuFlipClockInputs, GpuFlipClockParams, flags as clock_flags,
};
use super::gpu_flip_narrow_band::{BandPipelines, NarrowBand, NbParams};
use super::gpu_flip_pressure::{MAX_ITERATIONS, PressureSolver, Solve, Stop, Water, lattice_refusal, level_lattices};
mod read_params;
pub(crate) use read_params::{read_closed_faces, read_max_iterations, read_narrow_band, read_sheet_fill_rate, read_solve_level};
use read_params::{read_epoch, read_iterations};
use super::liquid_solid_distance::{SolidDistanceJob, encode_solid_distance};
use manifold_water_liquid::primitives::liquid_stats::{SOLVER_WORDS, with_stats_layout};
use manifold_water_liquid::primitives::prefix_scan::ScanLabels;
use manifold_water_liquid::float_param;
use manifold_water_liquid::primitives::sort_particles_into_cells::{LIQUID_PARTICLE_READ, ParticleSorter, SortJob, SortLabels, int_param};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::particles::FluidParticle;
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::ports::EXACT_F32_COUNT;
use manifold_water_liquid::WATER_DENSITY;
use manifold_water_liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape, MOBILITY_BYTES};
use manifold_nodes_physics::force_lattice::{ForceLatticeBinding, FORCE_LATTICE};
use manifold_water_liquid::lattice::{FlipSolverGrid, LiquidLattice};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

mod emit;
mod encode;
mod extend;
mod lattice;
mod narrow_band;
mod pipelines;
mod pockets;
mod primitive;
mod state;
mod sheeting;
mod tiles;

pub(crate) use emit::emit_sites;
use emit::encode_emit;
use extend::extend;
use lattice::{LatticeBuffers, allocate};
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(crate) use lattice::scratch_bytes;
use narrow_band::{NarrowHistory, narrow_params, narrow_redistance, narrow_uniform};
use pipelines::Pipelines;
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
use pipelines::step_source;
pub(crate) use pockets::POCKET_GATE_WORDS;
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) use pockets::pocket_rounds;
use pockets::{encode_pocket_coarsen, encode_pocket_mean, encode_pockets};
#[cfg(test)]
pub(crate) use pockets::atomic_sites_outside;
pub(super) use tiles::all_tiles;
use tiles::{TileTable, canonical, encode_tiles, retired_args_offset};
manifold_core::testkit_visible! { pub(crate) use tiles::{TILE, ring_max}; }
#[cfg(any(test, feature = "testkit"))]
pub use tiles::{tile_counts, tile_total};
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(crate) use tiles::tile_scratch_bytes;
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub use tiles::{CELL_REACH, set_all_tiles};
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub use extend::set_force_dense_extend;
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub use pockets::set_force_indirect_pockets;

manifold_core::testkit_visible! {
/// The step's kernels, joined in this order; a later file may use what an
/// earlier one declares.
pub(crate) const STEP_SHADER: &str = concat!(
    include_str!("shaders/gpu_flip_step/common.wgsl"),
    include_str!("shaders/gpu_flip_step/transfer.wgsl"),
    include_str!("shaders/gpu_flip_step/extend.wgsl"),
    include_str!("shaders/gpu_flip_step/forces.wgsl"),
    include_str!("shaders/gpu_flip_step/solids.wgsl"),
    include_str!("shaders/gpu_flip_step/pocket_bindings.wgsl"),
    include_str!("shaders/gpu_flip_step/tile_bindings.wgsl"),
    include_str!("shaders/gpu_flip_step/pockets.wgsl"),
    include_str!("shaders/gpu_flip_step/divergence.wgsl"),
    include_str!("shaders/gpu_flip_step/particle_distance.wgsl"),
    include_str!("shaders/gpu_flip_step/projection.wgsl"),
    include_str!("shaders/gpu_flip_step/density.wgsl"),
    include_str!("shaders/gpu_flip_step/move.wgsl"),
    include_str!("shaders/gpu_flip_step/emit.wgsl"),
    include_str!("shaders/gpu_flip_step/tiles.wgsl"),
    include_str!("shaders/gpu_flip_step/narrow_band.wgsl"),
    include_str!("shaders/gpu_flip_step/marker_removal.wgsl"),
);
}
const MASK_SHADER: &str = include_str!("shaders/gpu_flip_commit_mask.wgsl");
const NAME: &str = "GPU FLIP Step";

manifold_core::testkit_visible! {
/// Layers of valid faces the step's face grid holds around the water at
/// least: both extensions run `band_layers` ≥ 5.
pub const FACE_VALID_LAYERS: u32 = 2;
}
manifold_core::testkit_visible! {
/// Configured engine CFL, shared with the clock.
pub(crate) const ENGINE_CFL: u32 = 5;
}

/// A fenced maximum for the exact incoming marker state can prove that the
/// GPU scheduler's first step consumes the entire interval. Keep a 1% margin
/// from its ceil boundary for CPU/GPU length and division rounding. This only
/// decides whether to omit unused command chains; GPU CFL and max steps stay
/// unchanged. The caller excludes first-tick prediction and events, and the
/// obstacle bound must describe the same initial state with zero reaction.
fn one_step_cfl_safe(marker_speed: f32, obstacle_speed: f32, interval: f32, cell_size: f32) -> bool {
    marker_speed.is_finite() && marker_speed >= 0.0
        && obstacle_speed.is_finite() && obstacle_speed >= 0.0
        && interval.is_finite() && interval > 0.0
        && cell_size.is_finite() && cell_size > 0.0
        && f64::from(interval) * (f64::from(marker_speed.max(obstacle_speed)) + manifold_physics::stepping::LIVE_CFL_EPSILON) * 1.01
            < f64::from(ENGINE_CFL) * f64::from(cell_size)
}

manifold_core::testkit_visible! {
/// FLIP Fluids _extrapolateFluidVelocities: configured CFL, never travel.
pub(crate) fn band_layers(cfl: u32) -> u32 {
    (3f64.sqrt() * f64::from(cfl)).ceil() as u32 + 3
}
}


fn cell_bytes(cells: [u32; 3]) -> u64 {
    cells.iter().map(|&n| u64::from(n)).product::<u64>() * 4
}

/// Test-only lever, approved 2026-10-02 lead; un-suppressed when the executor
/// exposes node access. With it set, the step runs the poison entry
/// (gpu_flip_tile_tests.rs) after the retire: NaN into every cell array of
/// the tiles outside rings 0 and 1 (design section 4 (The defined-value
/// rule)).
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
static POISON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn set_poison(on: bool) {
    POISON.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// Test-only lever for the inactive-slot proof (BUG-e6z6s (inactive FLIP
/// slots)): with it set, every pass of an inactive clock slot runs as it did
/// before the slots were gated. The gate plan is zeros, so only the passes
/// that always read the clock see the slot as inactive.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
static GATE_OFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn set_gate_off(on: bool) {
    GATE_OFF.store(on, std::sync::atomic::Ordering::SeqCst);
}

pub(super) fn gating() -> bool {
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    {
        !GATE_OFF.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(all(any(test, feature = "testkit"), feature = "gpu-proofs")))]
    {
        true
    }
}

// The proof compares the no-body clear with the existing solid kernels.
// This switch and its load are absent from production builds.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
static FORCE_SOLID_VELOCITY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn set_force_solid_velocity(on: bool) {
    FORCE_SOLID_VELOCITY.store(on, std::sync::atomic::Ordering::SeqCst);
}

fn solid_velocity_needed(body_count: i32) -> bool {
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    if FORCE_SOLID_VELOCITY.load(std::sync::atomic::Ordering::SeqCst) {
        return true;
    }
    body_count > 0
}

manifold_core::testkit_visible! {
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
    /// Dispatches in each velocity-extension replay segment.
    pub(crate) extension_layers: u32,
    /// [`ring_max`] for this step: the table's extent.
    pub(crate) ring_max: u32,
    /// Ferstl 2016: 0 dense, 1 full-history initialization, 2 masked band.
    pub(crate) narrow_band: u32,
    pub(crate) live_impulse_stride: u32,
    /// Cells from the grid's minimum to each wall (`FlipSolverGrid::wall_inset`);
    /// 0 when the walls are the grid's edges.
    pub(crate) wall_inset: f32,
    /// Box3D's substep over this tick: the coupled motion law's h
    /// (`manifold_physics::coupled_motion::coupled_substep`).
    pub(crate) coupled_h: f32,
    pub(crate) _pad: [u32; 2],
}
}

/// One pass of the step's shader on its own, for the value proofs against
/// the CPU references: `entry` over `threads` threads, `buffers` at their
/// bindings, waited on.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn dispatch_pass(device: &GpuDevice, entry: &str, params: &StepParams, buffers: &[(u32, &GpuBuffer)], threads: u64) {
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

manifold_core::testkit_visible! {
#[derive(Default)]
pub(crate) struct StepState {
    identity_ops: manifold_water_liquid::primitives::particle_identity::ParticleIdentity,
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
    /// No-body solid scratch stays zero until a body pass writes it.
    solid_velocity_is_zero: bool,
    sorted: Option<GpuBuffer>,
    /// The emission flags' scan, one word a half-cell site.
    emit_scan: PrefixScan,
    /// The face grid output: exactly [`face_bytes`] of the current lattice.
    faces: Option<GpuBuffer>,
    /// [`ZERO_BYTES`] zero bytes bound where an optional input is unwired.
    saved_particles: Option<GpuBuffer>,
    saved_distance: Option<GpuBuffer>,
    saved_capped: Option<GpuBuffer>,
    saved_narrow_phi: Option<GpuBuffer>,
    saved_narrow_faces: Option<GpuBuffer>,
    saved_narrow_failure: Option<GpuBuffer>,
    zeros: Option<GpuBuffer>,
    /// This tick's bodies posed at the step's end (`pose_bodies`), one row a
    /// body of the tick.
    posed: Option<GpuBuffer>,
    /// Each posed body's packed response to an impulse while it stays on
    /// its supports (`pose_bodies`), six vec4 a body.
    mobility: Option<GpuBuffer>,
    bodies: BodyPasses,
    clock: Option<GpuFlipClock>,
    clock_capacities: [u32; 3],
    history: manifold_water_liquid::substep_history::SubstepHistory,
    narrow: NarrowBand,
    narrow_history: Option<NarrowHistory>,
    narrow_reset_pending: bool,
    narrow_full_count: bool,
    interior: Option<GpuBuffer>,
    /// Sheet seeding (BUG-j9l9w): the surface distance it reads, the stage,
    /// and each sorted slot's input index. Reserved only when the rate is on.
    surface: manifold_water_liquid::primitives::whitewater_distance::SurfaceDistance,
    sheeting: super::gpu_flip_sheeting::GpuSheeting,
    sheet_order: Option<GpuBuffer>,
}
}

/// Zero bytes bound for an unwired input: the uniform-sized arrays, and an
/// empty reaction for every body a liquid holds.
const ZERO_BYTES: u64 = (MAX_LIQUID_BODIES * REACTION_FLOATS * 4) as u64;

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

/// Layers of the solid velocity's extrapolation (gpu_flip_step/solids.wgsl
/// SOLID_LAYERS, MeshLevelSet::_numVelocityExtrapolationLayers).
const SOLID_LAYERS: u32 = 5;

/// Everything one step reads, resolved before any pass is encoded.
#[derive(Clone, Copy)]
struct Step<'a> {
    identity: &'a GpuBuffer,
    params: StepParams,
    clock_plan: &'a GpuBuffer,
    particles: &'a GpuBuffer,
    out: &'a GpuBuffer,
    /// Two words a slot: zero stage-cap count and refused push-outs; then the
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
    /// bodies' pose; added to when `dynamic`.
    reaction: &'a GpuBuffer,
    /// Each body row's support points at its tick's start
    /// (`liquid::bodies::BodySupports`); zeros unwired.
    contacts: &'a GpuBuffer,
    /// The bodies take part in the pressure solve and gather its reaction.
    dynamic: bool,
    /// When the pressure and density solves stop.
    pressure: Stop,
    /// The V-cycle level both solves' gradients run on; 0 is the fine
    /// lattice.
    level: usize,
    wall_inset: f32,
    band: u32,
    ghost: bool,
    /// Run the density projection.
    density: bool,
    narrow_enabled: bool,
    restore_narrow: bool,
    /// The sheet fill rate. At 0 no sheeting pass is encoded and no fresh
    /// scratch is reserved; the pipelines still prepare at install (so
    /// switching on mid-show does not hitch) and scratch reserved while it was
    /// on stays held.
    sheet_rate: f32,
}

manifold_node_engine::primitive! {
    name: GpuFlipStep,
    type_id: "node.gpu_flip_step",
    purpose: "Advance GPU FLIP water through the accepted clock interval with CFL 5 and Steps minimum substeps (1 by default); each substep sorts the particles into the lattice's cells, gathers their velocity onto the cell faces, adds gravity and the scene's forces and impulses, makes the water incompressible against the tank walls and the scene's solid bodies (a multigrid-preconditioned pressure solve, the free surface placed where the particles' distance crosses zero), moves crowded particles apart and sparse ones together to reduce marker density error (a density projection, position only, when Volume Projection is 1), then moves every particle through the new velocity, blending FLIP and PIC by Flip Share, and resolves its move against the walls and solid bodies (a particle still inside a solid is removed). A face Closed Faces leaves open (bit 2d the low face of axis d, bit 2d + 1 the high one) drains: every wall stays solid, and a particle that ends a substep within 2 cells of an open face is removed. Water a moving solid seals off from air (and from any open face) does not take that solid's push, unless the bodies are in the solve.When dynamic_bodies is above 0, each body that takes a reaction joins the pressure solve with its own velocity, so the water pushes it and it pushes back in the same solve, and the step adds the pressure's and the friction's impulse on every body to the reaction. Extrapolates projected velocity by 12 layers before constraining solids. Removes markers above 250 per cell and extreme velocities using the accepted interval, compacts survivors without changing their ids, then emits inflow for the next step. Outputs the moved particles, the step's face grid (valid at least 2 layers around the water) and the reaction, in place.",
    inputs: {
        particles: Array(FluidParticle) required,
        identity: Array(u32) required,
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
        retired_max_speed: ScalarF32 optional,
        interval_duration: ScalarF32 optional, limit_interval: ScalarF32 optional,
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        body_count: ScalarF32 optional,
        regions: Array(LiquidBody) optional,
        region_count: ScalarF32 optional,
        rows: ScalarF32 optional,
        reaction: Array(f32) optional,
        contacts: Array(f32) optional,
        dynamic_bodies: ScalarF32 optional,
        closed_faces: ScalarF32 optional,
        solve_level: ScalarF32 optional,
        max_iterations: ScalarF32 optional,
        sheet_fill_rate: ScalarF32 optional,
        clock_obstacles: Array(f32) optional,
        clock_obstacle_count: ScalarF32 optional,
        initial_obstacle_speed: ScalarF32 optional,
        clock_sources: Array(f32) optional,
        clock_source_count: ScalarF32 optional,
        live_hits: Array(f32) optional,
        live_hit_count: ScalarF32 optional,

        narrow_band: ScalarF32 optional,
        epoch: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
        identity_out: Array(u32),
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
        int_param!("body_count", "Bodies", 0.0, 0.0, MAX_LIQUID_BODIES as f32),
        int_param!("rows", "Rows", 0.0, 0.0, 16_777_216.0),
        int_param!("region_count", "Regions", 0.0, 0.0, i32::MAX as f32),
        float_param!("inflow_jitter", "Inflow Jitter", 0.0, 0.0, 1.0),
        int_param!("steps", "Steps", 1.0, 1.0, 64.0),
        float_param!("flip", "Flip Share", 0.95, 0.0, 1.0),
        int_param!("iterations", "Iterations (0 = Auto)", 0.0, 0.0, MAX_ITERATIONS as f32),
        int_param!("max_iterations", "Max Iterations", MAX_ITERATIONS as f32, 1.0, MAX_ITERATIONS as f32),
        int_param!("ghost_fluid", "Ghost Fluid", 1.0, 0.0, 1.0),
        int_param!("volume_projection", "Volume Projection", 0.0, 0.0, 1.0),
        int_param!("closed_faces", "Closed Faces", 63.0, 0.0, 63.0),
        int_param!("solve_level", "Solve Level", 0.0, 0.0, 4.0),
        int_param!("narrow_band", "Narrow Band", 0.0, 0.0, 1.0),
        // Shipped presets wire the domain's Sheet Fill Rate into the input, which wins;
        // only standalone graphs and tests reach this param.
        float_param!("sheet_fill_rate", "Sheet Fill Rate", 0.0, 0.0, 1.0),
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

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
impl GpuFlipStep {
    /// The sheet seeding stage, for its proofs.
    #[cfg(test)]
    pub(crate) fn sheeting(&self) -> &super::gpu_flip_sheeting::GpuSheeting {
        &self.state.sheeting
    }
}


#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(test)]
mod tests;

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
impl StepParams {
    pub fn capacity_for_test(&self) -> u32 { self.capacity }
    pub fn narrow_band_for_test(&self) -> u32 { self.narrow_band }
    pub fn cells_for_test(&self) -> [u32; 3] { self.n }
    pub fn step_dt_for_test(&self) -> f32 { self.step_dt }
}

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
impl StepParams {
    pub fn with_grid_for_test(mut self, n: [u32; 3], min: [f32; 3], cell_size: f32) -> Self {
        self.n = n; self.box_min = min; self.cell_size = cell_size; self
    }
    pub fn with_gather_for_test(mut self, capacity: u32, narrow_band: u32, step_dt: f32) -> Self {
        self.capacity = capacity; self.narrow_band = narrow_band; self.step_dt = step_dt; self
    }
    pub fn with_tiles_for_test(mut self, ring_max: u32, extension_layers: u32, all_tiles: u32) -> Self {
        self.ring_max = ring_max; self.extension_layers = extension_layers; self.all_tiles = all_tiles; self
    }
}
