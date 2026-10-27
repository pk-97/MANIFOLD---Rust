//! `node.gpu_flip_step` — one GPU FLIP water step: sort, particle distance, particles to faces,
//! extend, forces, solids, water mask from φ, divergence, pressure solve,
//! projection, extend, constraint, density projection, then the particles
//! move, remove crowded/extreme markers, compact, and emit inflow. One node
//! because no pass has a consumer outside the step and the solve between them
//! is a barriered reduction; the hand kernels live in
//! `shaders/gpu_flip_step/*.wgsl`, the solver in [`super::gpu_flip_pressure`].
//!
//! Every scratch array is sized from the pool (`pool_tiles`, the domain's
//! wire) and the particle slots
//! before any pass is encoded, so no kernel reads `arrayLength`. The step
//! always encodes [`history_slots`] substeps; each slot opens a record
//! `[groups, 1, 1, n]` of the slots in use (`shaders/gpu_flip_step/count.wgsl`,
//! the birth identity's words 4-7), and every pass over the particles
//! dispatches from it and stops at `n`, so a slot the clock plan leaves no
//! time for launches no particle work and writes nothing. The
//! lattice's cell and face fields are pooled per world tile (`tiles.rs`):
//! every active step builds the world tile table from the particles
//! (`manifold_water_liquid::world_tiles`), and every lattice pass runs over
//! its lists, 512 threads a slot, on every cell and record of a held tile.
//! The Bounds box is not a domain: its Wall faces are solids in the solid
//! distance beside the bodies, and the move removes water that ends beyond
//! an Open face's plane or below the kill height. The `faces` and `distance`
//! outputs are the pooled internal faces and
//! φ themselves, read through the `tiles` table and `pool_tiles`; they are
//! reallocated when the pool changes.
//!
//! Extrapolation order/count, marker removal, inflow emission/placement,
//! inflow constrained velocity and outflow removal port
//! FLIP Fluids `fluidsimulation.cpp` (MIT, Copyright (C) 2026 Ryan L. Guy &
//! Dennis Fassbaender; see THIRD_PARTY_NOTICES.md), line refs in the shader.
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

use manifold_water_liquid::primitives::prefix_scan::PrefixScan;
use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use super::gpu_flip_bodies::{Bodies, BodyPasses, REACTION_FLOATS, body_refusal};
use super::gpu_flip_clock::{
    GpuFlipClock, GpuFlipClockInputs, GpuFlipClockParams, flags as clock_flags,
};
use super::gpu_flip_pressure::{MAX_ITERATIONS, PressureSolver, Solve, Stop, Water};
mod read_params;
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_nodes_physics::force_lattice::{FORCE_LATTICE, ForceLatticeBinding};
use manifold_water_liquid::WATER_DENSITY;
use manifold_water_liquid::bodies::{
    LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape, MOBILITY_BYTES,
};
use manifold_water_liquid::float_param;
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::primitives::liquid_stats::{SOLVER_WORDS, with_stats_layout};
use manifold_water_liquid::primitives::prefix_scan::ScanLabels;
use manifold_water_liquid::primitives::sort_particles_into_cells::{
    LIQUID_PARTICLE_READ, ParticleSorter, SortJob, SortLabels, int_param,
};
use manifold_water_liquid::world_tiles::WorldTiles;
use read_params::read_iterations;
pub(crate) use read_params::{
    read_closed_faces, read_max_iterations, read_open_faces, read_sheet_fill_rate, read_solve_level,
};

mod encode;
mod extend;
mod lattice;
mod pipelines;
mod pockets;
mod primitive;
mod state;
use state::PlanReadback;
mod tiles;

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub use extend::set_force_dense_extend;
use extend::*;
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(crate) use lattice::scratch_bytes;
use lattice::*;
#[cfg(test)]
pub(crate) use pipelines::step_source_with;
use pipelines::*;
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub use pockets::set_force_indirect_pockets;
pub(crate) use pockets::*;
#[cfg(any(test, feature = "testkit"))]
pub use tiles::*;
#[cfg(not(any(test, feature = "testkit")))]
pub(crate) use tiles::*;

manifold_core::testkit_visible! { pub(crate) const STEP_SHADER: &str = concat!(
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
    include_str!("shaders/gpu_flip_step/count.wgsl"),
    include_str!("shaders/gpu_flip_step/tiles.wgsl"),
    include_str!("shaders/gpu_flip_step/marker_removal.wgsl"),
); }
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

/// Substeps the step encodes in a tick: the most the clock can schedule,
/// `LIVE_DEFAULT_MAX_STEPS`, and one more for each event inside the interval.
/// Both are CPU facts, so the encode never waits on a readback; the clock plan
/// decides on the GPU which of them run, and an idle slot launches no particle,
/// tile or solve work. The extent rule sizes the substep history with the same
/// count.
pub(crate) fn history_slots(live_hit_count: u32) -> u32 {
    manifold_physics::stepping::LIVE_DEFAULT_MAX_STEPS + live_hit_count
}

/// The numerical substeps a live tick encodes slots for: what the fastest
/// speed of the newest retired tick asks for at the CFL over `interval`,
/// half again so the speed can grow before its plan is read, at least
/// `minimum`. With no retired tick, every slot. An encoded slot the clock
/// leaves idle still costs its passes, and a tick that needs more than it
/// got stretches its last step over the rest of the frame.
pub(crate) fn live_steps(speed: Option<f32>, interval: f32, cell_size: f32, minimum: u32) -> u32 {
    let max = manifold_physics::stepping::LIVE_DEFAULT_MAX_STEPS;
    let Some(speed) = speed.filter(|s| s.is_finite() && *s >= 0.0) else {
        return max;
    };
    let wanted = (1.5 * speed * interval / (ENGINE_CFL as f32 * cell_size)).ceil();
    (wanted.min(max as f32) as u32).clamp(minimum.clamp(1, max), max)
}

manifold_core::testkit_visible! {
/// FLIP Fluids _extrapolateFluidVelocities: configured CFL, never travel.
pub(crate) fn band_layers(cfl: u32) -> u32 {
    (3f64.sqrt() * f64::from(cfl)).ceil() as u32 + 3
}
}

/// The step's extension layers: the engine's band at its CFL, never under the
/// face grid's guarantee. The world tiles reach [`ring_max`] of it.
pub(crate) fn step_band() -> u32 {
    band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS)
}

/// Test-only lever. With it set, the step runs the poison entry
/// (gpu_flip_tile_tests.rs) after the retire: NaN into every cell array of
/// the held slots outside rings 0 and 1 (design section 4 (The defined-value
/// rule)), and NaN into every pooled field of the free slots.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
static POISON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn set_poison(on: bool) {
    POISON.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// Test-only lever for the idle-slot proof (approved by the lead): the slots
/// the step encodes in place of [`history_slots`], 0 for none set. A slot the
/// clock leaves no time for must write nothing, so a scene the clock finishes
/// in its first slot comes out the same at one slot as at the full count.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
static ENCODED_SLOTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn set_encoded_slots(slots: Option<u32>) {
    ENCODED_SLOTS.store(slots.unwrap_or(0), std::sync::atomic::Ordering::SeqCst);
}

/// `rule` slots, or the proofs' lever's.
fn encoded_slots(rule: u32) -> u32 {
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    {
        match ENCODED_SLOTS.load(std::sync::atomic::Ordering::SeqCst) {
            0 => rule,
            slots => slots,
        }
    }
    #[cfg(not(all(any(test, feature = "testkit"), feature = "gpu-proofs")))]
    {
        rule
    }
}

// The proof compares the no-body clear with the existing solid kernels.
// This switch and its load are absent from production builds.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
static FORCE_SOLID_VELOCITY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct StepParams {
    /// The Bounds box in metres as authored: the planes of its Wall and Open
    /// faces.
    pub(crate) bounds_lo: [f32; 3],
    pub(crate) capacity: u32,
    pub(crate) bounds_hi: [f32; 3],
    pub(crate) cell_size: f32,
    pub(crate) gravity: [f32; 3],
    pub(crate) step_dt: f32,
    pub(crate) field_nodes: [u32; 3],
    pub(crate) field_spacing: f32,
    /// The lattice origin (`lattice_origin`).
    pub(crate) origin: [f32; 3],
    /// `1.0 / cell_size`, the value the sort bins by (`SortJob::inv_cell`).
    pub(crate) inv_cell: f32,
    pub(crate) tick_index: i32,
    pub(crate) step_in_tick: i32,
    pub(crate) force_lattices: i32,
    pub(crate) impulse_tick: i32,
    pub(crate) first_tick: i32,
    pub(crate) body_count: i32,
    pub(crate) rows: i32,
    pub(crate) tick_seconds: f32,
    pub(crate) flip: f32,
    pub(crate) ghost: u32,
    pub(crate) particles: u32,
    pub(crate) shapes_len: u32,
    pub(crate) rate: f32,
    /// The Bounds faces that are solid walls, bit 2d the low face of axis d
    /// and bit 2d + 1 the high one.
    pub(crate) wall_faces: u32,
    pub(crate) region_count: i32,
    pub(crate) region_rows: i32,
    /// Half-width of an emitted particle's jitter in cells.
    pub(crate) emit_jitter: f32,
    /// The V-cycle level the pressure solves run on (`Step::level`).
    pub(crate) solve_level: u32,
    /// Dispatches in each velocity-extension replay segment.
    pub(crate) extension_layers: u32,
    /// [`ring_max`] of the step's band: the table's reach.
    pub(crate) ring_max: u32,
    pub(crate) live_impulse_stride: u32,
    /// The Bounds faces that remove water ending beyond their plane, same
    /// bits.
    pub(crate) open_faces: u32,
    /// Box3D's substep over this tick: the coupled motion law's h
    /// (`manifold_physics::coupled_motion::coupled_substep`).
    pub(crate) coupled_h: f32,
    /// Tiles the pool holds ([`pool_tiles`]).
    pub(crate) pool_tiles: u32,
    /// The force and impulse lattices' first node in metres.
    pub(crate) field_min: [f32; 3],
    /// The table's entries.
    pub(crate) table_entries: u32,
    /// Sites the inflows of one tick enumerate at most (`region_site_total`).
    pub(crate) emit_sites: u32,
    /// The move removes a particle that ends below this height in metres;
    /// `f32::MIN` removes none.
    pub(crate) kill_height: f32,
    pub(crate) _pad: [u32; 2],
    /// Each Bounds face plane's velocity along its axis in m/s, the low
    /// faces then the high: what a moving Wall gives the water it touches.
    pub(crate) wall_velocity_lo: [f32; 3],
    pub(crate) _pad_lo: u32,
    pub(crate) wall_velocity_hi: [f32; 3],
    pub(crate) _pad_hi: u32,
}
}

/// Free space: no Bounds, no walls, no open faces and no kill height.
impl Default for StepParams {
    fn default() -> Self {
        Self {
            kill_height: f32::MIN,
            ..bytemuck::Zeroable::zeroed()
        }
    }
}

/// One pass of the step's shader on its own, for the value proofs against
/// the CPU references: `entry` over `threads` threads, `buffers` at their
/// bindings, waited on.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn dispatch_pass(
    device: &GpuDevice,
    entry: &str,
    params: &StepParams,
    buffers: &[(u32, &GpuBuffer)],
    threads: u64,
) {
    let pipeline = device.create_compute_pipeline(&step_source(), entry, "node.gpu_flip_step");
    let mut bindings = vec![uniform(params)];
    bindings.extend(buffers.iter().map(|&(binding, b)| buffer(binding, b)));
    let clock_plan = device.create_buffer_shared(48);
    clock_plan.zero_fill();
    if !buffers.iter().any(|&(binding, _)| binding == 46) {
        bindings.push(buffer(46, &clock_plan));
    }
    // The slots in use are the threads asked for, as a pass that dispatches
    // from the step's record would find them.
    let record = device.create_buffer_shared(16);
    let slots = u32::try_from(threads).expect("thread count fits a word");
    // SAFETY: a shared buffer of four words; no GPU work has touched it yet.
    unsafe { record.write(0, bytemuck::cast_slice(&[groups(threads)[0], 1, 1, slots])) };
    if !buffers.iter().any(|&(binding, _)| binding == 42) {
        bindings.push(buffer(42, &record));
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
    sorter: ParticleSorter,
    solver: PressureSolver,
    lattice: Option<LatticeBuffers>,
    /// The world tiles over the lattice's pool, kept with `lattice`. One slot
    /// from install until the first reserve, so the build's pipelines compile
    /// at install.
    tiles: Option<WorldTiles>,
    /// Under [`set_all_tiles`]: one point in every tile of the box, the
    /// table's points in place of the particles, and their record.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    box_points: Option<(GpuBuffer, GpuBuffer)>,
    /// A lattice was just built: its pooled fields' canonical elements are
    /// written by the next step (`pool_canon`).
    pool_canon_pending: bool,
    /// No-body solid scratch stays zero until a body pass writes it.
    solid_velocity_is_zero: bool,
    sorted: Option<GpuBuffer>,
    /// The emission flags' scan, one word a site of the tick's enumeration.
    emit_scan: PrefixScan,
    /// The slot's record `[groups, 1, 1, n]` (`count.wgsl`): written on the GPU
    /// at the start of every slot, read by every pass over the particles.
    slot_record: Option<GpuBuffer>,
    /// [`ZERO_BYTES`] zero bytes bound where an optional input is unwired.
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
    /// Each live tick's last clock plan, copied for the CPU until its frame
    /// retires (`state.rs`).
    plan_readbacks: Vec<PlanReadback>,
    /// The fastest speed the newest retired tick's clock measured.
    retired_speed: Option<f32>,
    /// Sheet seeding (BUG-j9l9w): the true distance it reads, the stage,
    /// and each sorted slot's input index. Reserved only when the rate is on.
    sheet_distance: super::gpu_flip_sheet_distance::SheetDistance,
    sheeting: super::gpu_flip_sheeting::GpuSheeting,
    sheet_order: Option<GpuBuffer>,
}
}

/// Zero bytes bound for an unwired input: the uniform-sized arrays, and an
/// empty reaction for every body a liquid holds.
const ZERO_BYTES: u64 = (MAX_LIQUID_BODIES * REACTION_FLOATS * 4) as u64;

/// Bytes of the slot record `[groups, 1, 1, n]`.
const SLOT_RECORD_BYTES: u64 = 16;

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
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
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
    forces: &'a GpuBuffer,
    impulses: &'a GpuBuffer,
    bodies: &'a GpuBuffer,
    shapes: &'a GpuBuffer,
    atlas: &'a GpuBuffer,
    /// Inflow and outflow rows; read when `params.region_count` > 0.
    regions: &'a GpuBuffer,
    /// Each region row's half-cell site box (`gpu_flip_domain` RegionSites),
    /// `params.emit_sites` sites a tick at most.
    region_sites: &'a GpuBuffer,
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
    band: u32,
    ghost: bool,
    /// Run the density projection.
    density: bool,
    /// The sheet fill rate. At 0 no sheeting pass is encoded and no fresh
    /// scratch is reserved; the pipelines still prepare at install (so
    /// switching on mid-show does not hitch) and scratch reserved while it was
    /// on stays held.
    sheet_rate: f32,
}

impl StepState {}

manifold_node_engine::primitive! {
    name: GpuFlipStep,
    type_id: "node.gpu_flip_step",
    purpose: "Advance GPU FLIP water through the accepted clock interval with CFL 5 and Steps minimum substeps (1 by default); each substep sorts the particles into the lattice's cells, gathers their velocity onto the cell faces, adds gravity and the scene's forces and impulses, makes the water incompressible against the Bounds walls and the scene's solid bodies (a multigrid-preconditioned pressure solve, the free surface placed where the particles' distance crosses zero), moves crowded particles apart and sparse ones together to reduce marker density error (a density projection, position only, when Volume Projection is 1), then moves every particle through the new velocity, blending FLIP and PIC by Flip Share, and resolves its move against the walls and solid bodies (a particle still inside a solid is pushed out, never removed). The Bounds box is bounds_lo to bounds_hi in metres as authored: each face Closed Faces names (bit 2d the low face of axis d, bit 2d + 1 the high one) is a wall, a solid like a body; each face Open Faces names removes the water that ends a substep beyond its plane; water that ends a substep below Kill Height is removed too. With no wall, no open face and no kill height the water falls through free space. Water a moving solid seals off from air does not take that solid's push, unless the bodies are in the solve.When dynamic_bodies is above 0, each body that takes a reaction joins the pressure solve with its own velocity, so the water pushes it and it pushes back in the same solve, and the step adds the pressure's and the friction's impulse on every body to the reaction. Extrapolates projected velocity by 12 layers before constraining solids. Removes markers above 250 per cell and extreme velocities using the accepted interval, compacts survivors without changing their ids, then emits inflow for the next step. Outputs the moved particles, the step's face grid (valid at least 2 layers around the water) and the reaction, in place.",
    inputs: {
        particles: Array(FluidParticle) required,
        identity: Array(u32) required,
        cell_size: ScalarF32 optional,
        pool_tiles: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity_y: ScalarF32 optional, gravity_z: ScalarF32 optional,
        forces: Array(f32) optional,
        impulses: Array(f32) optional,
        field_nodes_x: ScalarF32 optional, field_nodes_y: ScalarF32 optional, field_nodes_z: ScalarF32 optional,
        field_spacing: ScalarF32 optional,
        force_lattices: ScalarF32 optional,
        impulse_tick: ScalarF32 optional,
        first_tick: ScalarF32 optional,
        tick_index: ScalarF32 optional,
        interval_duration: ScalarF32 optional, limit_interval: ScalarF32 optional,
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        body_count: ScalarF32 optional,
        regions: Array(LiquidBody) optional,
        region_count: ScalarF32 optional,
        region_sites: Array(u32) optional,
        region_site_total: ScalarF32 optional,
        rows: ScalarF32 optional,
        reaction: Array(f32) optional,
        contacts: Array(f32) optional,
        dynamic_bodies: ScalarF32 optional,
        bounds_lo_x: ScalarF32 optional, bounds_lo_y: ScalarF32 optional, bounds_lo_z: ScalarF32 optional,
        bounds_hi_x: ScalarF32 optional, bounds_hi_y: ScalarF32 optional, bounds_hi_z: ScalarF32 optional,
        wall_velocity_lo_x: ScalarF32 optional, wall_velocity_lo_y: ScalarF32 optional, wall_velocity_lo_z: ScalarF32 optional,
        wall_velocity_hi_x: ScalarF32 optional, wall_velocity_hi_y: ScalarF32 optional, wall_velocity_hi_z: ScalarF32 optional,
        closed_faces: ScalarF32 optional,
        open_faces: ScalarF32 optional,
        kill_height: ScalarF32 optional,
        solve_level: ScalarF32 optional,
        max_iterations: ScalarF32 optional,
        sheet_fill_rate: ScalarF32 optional,
        clock_obstacles: Array(f32) optional,
        clock_obstacle_count: ScalarF32 optional,
        clock_sources: Array(f32) optional,
        clock_source_count: ScalarF32 optional,
        live_hits: Array(f32) optional,
        live_hit_count: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
        identity_out: Array(u32),
        // The pooled internal faces (`pool_len(pool_tiles)` records).
        faces: Array(FaceSample),
        // The pooled distance (`pool_len(pool_tiles)` values).
        distance: Array(f32),
        // The world tile table of both pooled outputs (`table_words(pool_tiles)` words).
        tiles: Array(u32),
        // The pool's capacity in tiles, `P`.
        pool_tiles: ScalarF32,
        substep_schedule: Array(f32),
        // The substeps' pooled faces: slots · pool_len(pool_tiles) · 4 floats, all on `tiles`.
        substep_faces: Array(f32),
        substep_count: ScalarF32,
        reaction_out: Array(f32),
        capped: Array(u32),
        clock_status: Array(u32),
    },
    params: [
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        int_param!("pool_tiles", "Pool Tiles", 1728.0, 1.0, 16_777_216.0),
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
        float_param!("bounds_lo_x", "Bounds Min X", 0.0, -1.0e6, 1.0e6),
        float_param!("bounds_lo_y", "Bounds Min Y", 0.0, -1.0e6, 1.0e6),
        float_param!("bounds_lo_z", "Bounds Min Z", 0.0, -1.0e6, 1.0e6),
        float_param!("bounds_hi_x", "Bounds Max X", 0.0, -1.0e6, 1.0e6),
        float_param!("bounds_hi_y", "Bounds Max Y", 0.0, -1.0e6, 1.0e6),
        float_param!("bounds_hi_z", "Bounds Max Z", 0.0, -1.0e6, 1.0e6),
        float_param!("wall_velocity_lo_x", "Wall Velocity −X (m/s)", 0.0, -1.0e3, 1.0e3),
        float_param!("wall_velocity_lo_y", "Wall Velocity −Y (m/s)", 0.0, -1.0e3, 1.0e3),
        float_param!("wall_velocity_lo_z", "Wall Velocity −Z (m/s)", 0.0, -1.0e3, 1.0e3),
        float_param!("wall_velocity_hi_x", "Wall Velocity +X (m/s)", 0.0, -1.0e3, 1.0e3),
        float_param!("wall_velocity_hi_y", "Wall Velocity +Y (m/s)", 0.0, -1.0e3, 1.0e3),
        float_param!("wall_velocity_hi_z", "Wall Velocity +Z (m/s)", 0.0, -1.0e3, 1.0e3),
        int_param!("closed_faces", "Closed Faces (bits −X +X −Y +Y −Z +Z)", 0.0, 0.0, 63.0),
        int_param!("open_faces", "Open Faces (bits −X +X −Y +Y −Z +Z)", 0.0, 0.0, 63.0),
        float_param!("kill_height", "Kill Height", f32::MIN, f32::MIN, f32::MAX),
        int_param!("solve_level", "Solve Level", 0.0, 0.0, 4.0),
        // Shipped presets wire the domain's Sheet Fill Rate into the input, which wins;
        // only standalone graphs and tests reach this param.
        float_param!("sheet_fill_rate", "Sheet Fill Rate", 0.0, 0.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Inside node.liquid_state's tick region, once per tick: particles from the state's out, Steps substeps of 1/(60·Steps) s run inside the node, each moving the last one's particles, and the bodies see every substep. The cell size and pool_tiles, gravity, the field scalars, forces, impulses, bodies, shapes, atlas, body_count and body_rows (into rows) come from node.gpu_flip_domain; so do dynamic_bodies and reaction, which every substep adds to in place; tick_index from node.liquid_state. Flip Share is the share kept per 1/60 s, so the damping does not change with the step count. faces, distance, tiles and pool_tiles feed the whitewater stage; out keeps the particles slots. regions and region_count also come from node.gpu_flip_domain: each step an inflow seeds particles at its empty half-cell sites into free pool slots (a full pool emits nothing) and holds the velocity inside it, and an outflow kills the particles it holds; the live count rides the cell ranges. A pool the device cannot hold is a named error.",
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

#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
impl StepParams {
    pub fn capacity_for_test(&self) -> u32 {
        self.capacity
    }
    pub fn step_dt_for_test(&self) -> f32 {
        self.step_dt
    }
}

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
impl StepParams {
    /// A lattice at `cell_size` whose force lattices start at `min` (metres):
    /// the lattice origin, the inverse cell size and the field origin.
    pub fn with_grid_for_test(mut self, min: [f32; 3], cell_size: f32) -> Self {
        self.field_min = min;
        self.cell_size = cell_size;
        self.inv_cell = 1.0 / cell_size;
        self.origin = manifold_node_engine::scene::world_tiles::lattice_origin(cell_size);
        self
    }

    /// The Bounds box from `lo` to `hi` (metres) with its Wall and Open face
    /// masks.
    pub fn with_bounds_for_test(
        mut self,
        lo: [f32; 3],
        hi: [f32; 3],
        wall_faces: u32,
        open_faces: u32,
    ) -> Self {
        self.bounds_lo = lo;
        self.bounds_hi = hi;
        self.wall_faces = wall_faces;
        self.open_faces = open_faces;
        self
    }

    /// The pool and table the passes read through.
    pub fn with_pool_for_test(mut self, pool_tiles: u32) -> Self {
        self.pool_tiles = pool_tiles;
        self.table_entries = manifold_node_engine::scene::world_tiles::table_entries(pool_tiles);
        self
    }

    /// The particle slots the cell passes clamp a cell's range to.
    pub fn with_capacity_for_test(mut self, capacity: u32) -> Self {
        self.capacity = capacity;
        self
    }
    pub fn with_tiles_for_test(mut self, ring_max: u32, extension_layers: u32) -> Self {
        self.ring_max = ring_max;
        self.extension_layers = extension_layers;
        self
    }
}
