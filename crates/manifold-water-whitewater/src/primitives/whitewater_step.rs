//! `node.whitewater_step` — FLIP whitewater emission and lifecycle.
//! GPU FLIP runs this stage once per liquid tick using solver particle phi.
//! The liquid boundary captures the pool, IDs, counters and rendering arrays
//! after each tick; the stage owns reusable scratch. Every grid array lives on
//! the liquid step's world tiles, read through its table.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use manifold_gpu::BufferUse;
use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};
use manifold_water_liquid::fluid_particles::WhitewaterSpawn;

use super::emission_count::WAVECREST_RATE;
use super::energy_potential::{MAX_ENERGY, MIN_ENERGY};
use super::extend_lattice::ExtendLattice;
use super::keep_whitewater::KeepWhitewater;
use super::lattice_curvature::LatticeCurvature;
use super::preserve_foam::PreserveFoam;
use super::whitewater_influence::WhitewaterInfluence;
use super::whitewater_obstacle_source::WhitewaterSource;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::scene::world_tiles::{
    READ_SIDE, SLOT_WORDS, lattice_origin,
};
use manifold_nodes_physics::force_lattice::ForceLatticeBinding;
use manifold_physics::clock::TICK;
use manifold_water_liquid::bodies::{LiquidBody, LiquidShape};
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::primitives::liquid_cells::LiquidCells;
use manifold_water_liquid::primitives::particle_identity::{COUNT_OFFSET, IDENTITY_BYTES};
use manifold_water_liquid::primitives::prefix_scan::{PrefixScan, ScanLabels, storage_words};
use manifold_water_liquid::primitives::sort_particles_into_cells::{
    ParticleSorter, SortCount, SortJob, SortLabels, key_storage_bytes, whitewater_record_read,
};
use manifold_water_liquid::primitives::whitewater_distance::{SurfaceDistance, scratch_bytes};
use manifold_water_liquid::tile_pool::{TilePool, tile_pool};
use manifold_water_liquid::whitewater::{
    DEFAULT_CAPACITY, KnownValue, MAX_CAPACITY, WhitewaterParticle,
};
use manifold_water_liquid::{float_param, int_param};

const WHITEWATER_FUSED_SHADER: &str = include_str!("shaders/whitewater_fused.wgsl");
const FUSED_ENTRIES: [(&str, &str); 5] = [
    ("ww_emit", "node.whitewater_step.emit"),
    ("ww_dust", "node.whitewater_step.dust"),
    ("ww_spawn", "node.whitewater_step.spawn"),
    ("ww_lifecycle", "node.whitewater_step.lifecycle"),
    ("ww_turbulence", "node.whitewater_step.turbulence"),
];

/// Restarts the influence of the pool slots the table gave to another tile.
const RESTART_ENTRY: (&str, &str) = (
    "restart_moved_influence",
    "node.whitewater_step.restart_influence",
);

/// Keeps the table's slot rows for next frame's restart.
const SAVE_ROWS_ENTRY: (&str, &str) = ("save_slot_rows", "node.whitewater_step.save_rows");

#[derive(Clone, Copy)]
enum Fused {
    Emit,
    Dust,
    Spawn,
    Lifecycle,
    Turbulence,
}

impl Fused {
    const fn index(self) -> usize {
        self as usize
    }
}

fn fused_source() -> String {
    use manifold_nodes_physics::force_lattice::FORCE_LATTICE;
    use manifold_water_liquid::grid::LIQUID_FACES;
    use manifold_water_liquid::whitewater::WHITEWATER_COMMON;
    format!(
        "{READ_SIDE}\n{WHITEWATER_COMMON}\n{LIQUID_FACES}\n{FORCE_LATTICE}\n{WHITEWATER_FUSED_SHADER}"
    )
}

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
mod reference;

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn reference_proof_node() -> Box<dyn manifold_node_engine::exec::effect_node::EffectNode> {
    let mut node = WhitewaterStep::new();
    node.step.reference.enabled = true;
    node.step.reference.capture = true;
    Box::new(node)
}

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn fused_proof_node() -> Box<dyn manifold_node_engine::exec::effect_node::EffectNode> {
    let mut node = WhitewaterStep::new();
    node.step.reference.capture = true;
    Box::new(node)
}

#[cfg(any(test, feature = "testkit"))]
pub mod fused_tests;

// The production expansion contains only the fused block: no selector field,
// runtime flag, or reference branch is compiled into a shipping build.
macro_rules! emitter_path {
    ($reference:expr, $oracle:block, $fused:block) => {{
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        { if $reference $oracle else $fused }
        #[cfg(not(all(any(test, feature = "testkit"), feature = "gpu-proofs")))]
        $fused
    }};
}

/// `whitewater_step.wgsl`'s influence restart uniform: the pool, the
/// elements to visit (a slot's worth each, or a slot row's word each when
/// saving the rows) and the level a restarted element takes, padded to 16
/// bytes. The table's H is in its header.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RestartParams {
    pool_tiles: u32,
    _pad_h: u32,
    count: u32,
    base_level: f32,
}

/// The fused turbulence kernel's uniform, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TurbulenceParams {
    cell_size: f32,
    pool_tiles: f32,
    count: u32,
    _pad0: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EmitParams {
    cell_size: f32,
    pool_tiles: f32,
    bounds_lo_z: f32,
    seed: f32,
    epoch: f32,
    spray_speed: f32,
    min_energy: f32,
    max_energy: f32,
    min_curvature: f32,
    max_curvature: f32,
    sharpness: f32,
    min_turbulence: f32,
    max_turbulence: f32,
    inside_enabled: f32,
    rate: f32,
    turbulence_rate: f32,
    generation_rate: f32,
    points_per_cell: f32,
    ticks: f32,
    dt: f32,
    dust_enabled: f32,
    boundary_dust: f32,
    _pad0: f32,
    _pad1: f32,
}

impl EmitParams {
    fn new(frame: &StepFrame, dust: bool) -> Self {
        let s = &frame.shape;
        Self {
            cell_size: s.pool.cell_size,
            pool_tiles: s.pool.pool_tiles as f32,
            bounds_lo_z: frame.bounds_lo[2],
            seed: if dust {
                frame.seed + 104729.0
            } else {
                frame.seed
            },
            epoch: frame.epoch as f32,
            spray_speed: frame.spray_speed,
            min_energy: frame.min_energy,
            max_energy: frame.max_energy,
            min_curvature: super::wavecrest_potential::MIN_CURVATURE,
            max_curvature: super::wavecrest_potential::MAX_CURVATURE,
            sharpness: super::wavecrest_potential::SHARPNESS,
            min_turbulence: frame.min_turbulence,
            max_turbulence: frame.max_turbulence,
            inside_enabled: f32::from(u8::from(frame.inside_emission)),
            rate: if dust { 0.0 } else { frame.wavecrest_emission },
            turbulence_rate: if dust {
                frame.dust_rate
            } else {
                frame.turbulence_emission
            },
            generation_rate: frame.generation_rate,
            points_per_cell: 8.0,
            ticks: frame.ticks as f32,
            dt: frame.dt,
            dust_enabled: f32::from(u8::from(frame.dust_emission)),
            boundary_dust: f32::from(u8::from(frame.boundary_dust)),
            _pad0: 0.0,
            _pad1: 0.0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpawnParams {
    capacity: f32,
    seed: f32,
    epoch: f32,
    min_lifetime: f32,
    max_lifetime: f32,
    lifetime_variance: f32,
    dt: f32,
    spray_speed: f32,
    type_seed: f32,
    type_epoch: f32,
    dust: f32,
    cell_size: f32,
    pool_tiles: f32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LifecycleParams {
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    dt: f32,
    foam_advection: f32,
    bubble_buoyancy: f32,
    bubble_drag: f32,
    spray_drag: f32,
    spray_drag_variance: f32,
    spray_restitution: f32,
    spray_friction: f32,
    substep_count: f32,
    field_nodes_x: f32,
    field_nodes_y: f32,
    field_nodes_z: f32,
    field_spacing: f32,
    force_lattices: f32,
    tick_index: f32,
    first_tick: f32,
    cell_size: f32,
    pool_tiles: f32,
    bubble_lifetime_modifier: f32,
    foam_lifetime_modifier: f32,
    spray_lifetime_modifier: f32,
    count: u32,
}

impl SpawnParams {
    fn new(frame: &StepFrame, dust: bool) -> Self {
        let s = &frame.shape;
        Self {
            capacity: s.capacity as f32,
            seed: if dust {
                frame.seed + 104729.0
            } else {
                frame.seed
            },
            epoch: frame.epoch as f32,
            min_lifetime: super::spawn_whitewater::MIN_LIFETIME,
            max_lifetime: super::spawn_whitewater::MAX_LIFETIME,
            lifetime_variance: super::spawn_whitewater::LIFETIME_VARIANCE,
            dt: frame.dt,
            spray_speed: if dust { 1.0 } else { frame.spray_speed },
            type_seed: if dust { 0.0 } else { frame.seed },
            type_epoch: if dust { 0.0 } else { frame.epoch as f32 },
            dust: f32::from(u8::from(dust)),
            cell_size: s.pool.cell_size,
            pool_tiles: s.pool.pool_tiles as f32,
            count: s.capacity,
            _pad0: 0,
            _pad1: 0,
        }
    }
}

impl LifecycleParams {
    fn new(frame: &StepFrame, inputs: &StepInputs<'_>) -> Self {
        use super::{advect_whitewater as advect, age_whitewater as age};
        let s = &frame.shape;
        let [bounds_lo_x, bounds_lo_y, bounds_lo_z] = frame.bounds_lo;
        let [gravity_x, gravity_y, gravity_z] = frame.gravity;
        let motion = inputs.motion.as_ref();
        let [field_nodes_x, field_nodes_y, field_nodes_z] =
            motion.map_or([2; 3], |m| m.fields.nodes).map(|n| n as f32);
        Self {
            bounds_lo_x,
            bounds_lo_y,
            bounds_lo_z,
            gravity_x,
            gravity_y,
            gravity_z,
            dt: frame.dt,
            foam_advection: advect::FOAM_ADVECTION,
            bubble_buoyancy: advect::BUBBLE_BUOYANCY,
            bubble_drag: advect::BUBBLE_DRAG,
            spray_drag: advect::SPRAY_DRAG,
            spray_drag_variance: advect::SPRAY_DRAG_VARIANCE,
            spray_restitution: advect::SPRAY_RESTITUTION,
            spray_friction: advect::SPRAY_FRICTION,
            substep_count: motion.map_or(0.0, |m| m.count as f32),
            field_nodes_x,
            field_nodes_y,
            field_nodes_z,
            field_spacing: motion.map_or(0.25, |m| m.fields.spacing),
            force_lattices: motion.map_or(0.0, |m| m.fields.force_lattices as f32),
            tick_index: motion.map_or(0.0, |m| m.tick_index),
            first_tick: motion.map_or(0.0, |m| m.fields.first_tick as f32),
            cell_size: s.pool.cell_size,
            pool_tiles: s.pool.pool_tiles as f32,
            bubble_lifetime_modifier: age::BUBBLE_LIFETIME_MODIFIER,
            foam_lifetime_modifier: age::FOAM_LIFETIME_MODIFIER,
            spray_lifetime_modifier: age::SPRAY_LIFETIME_MODIFIER,
            count: s.capacity,
        }
    }
}

manifold_core::testkit_visible! {
pub(crate) const WHITEWATER_STEP_SHADER: &str = include_str!("shaders/whitewater_step.wgsl");
}

pub(crate) const OUTPUTS: [&str; 4] = [
    "foam_particles",
    "bubble_particles",
    "spray_particles",
    "dust_particles",
];

/// Words the GPU writes per output slot: foam, bubble and spray counts,
/// emitted, thinned, pool full, live, next id, dust count (first eight preserved).
const COUNT_WORDS: usize = 9;
const STATE_WORDS: u64 = 8;

const PARTICLE: u64 = std::mem::size_of::<FluidParticle>() as u64;
const POOL_SLOT: u64 = std::mem::size_of::<WhitewaterParticle>() as u64;
const SPAWN: u64 = std::mem::size_of::<WhitewaterSpawn>() as u64;
const KNOWN_VALUE: u64 = std::mem::size_of::<KnownValue>() as u64;
const FACE_RECORD: u64 = std::mem::size_of::<FaceSample>() as u64;
const SOURCE: u64 = std::mem::size_of::<WhitewaterSource>() as u64;
/// Bytes of one pooled record of the substep face history: x, y, z and a zero.
const HISTORY_RECORD: u64 = 16;

manifold_node_engine::primitive! {
    name: WhitewaterStep,
    type_id: "node.whitewater_step",
    purpose: "With distance and pool state wired from a liquid tick region, emit and advance exactly one tick and publish through captured boundary results. Emit from where the surface crests and the water moves fast, then step the pool (spray falls and bounces, bubbles rise and drag, foam rides the surface, each ages, dies, and leaves when it strays or crowds its cell), each particle's radius its fade. Every grid it reads or keeps lives on the liquid step's world tiles. Spawns past the pool's room are counted as pool full; emissions past Capacity in one frame are thinned evenly and counted.",
    inputs: {
        particles: Array(FluidParticle) required,
        identity: Array(u32) required,
        capacity: ScalarF32 optional,
        solid: Array(f32) required,
        obstacle_source: Array(WhitewaterSource) optional,
        faces: Array(FaceSample) required,
        distance: Array(f32) required,
        tiles: Array(u32) required,
        pool_tiles: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        bounds_lo_x: ScalarF32 optional, bounds_lo_y: ScalarF32 optional, bounds_lo_z: ScalarF32 optional,
        bounds_hi_x: ScalarF32 optional, bounds_hi_y: ScalarF32 optional, bounds_hi_z: ScalarF32 optional,
        open_faces: ScalarF32 optional,
        kill_height: ScalarF32 optional,
        substep_schedule: Array(f32) optional,
        substep_faces: Array(f32) optional,
        substep_count: ScalarF32 optional,
        forces: Array(f32) optional, impulses: Array(f32) optional,
        field_nodes_x: ScalarF32 optional, field_nodes_y: ScalarF32 optional, field_nodes_z: ScalarF32 optional,
        field_spacing: ScalarF32 optional, force_lattices: ScalarF32 optional,
        impulse_tick: ScalarF32 optional, first_tick: ScalarF32 optional,
        regions: Array(LiquidBody) optional, shapes: Array(LiquidShape) optional, atlas: Array(u32) optional,
        collider_nodes: Array(manifold_node_engine::mesh::Vec4Vertex) optional, collider_triangles: Array(manifold_node_engine::mesh::Vec4Vertex) optional,
        region_count: ScalarF32 optional,
        pool: Array(WhitewaterParticle) optional,
        pool_state: Array(u32) optional,
        tick_index: ScalarF32 optional,
        dt: ScalarF32 optional,
        epoch: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
    },
    outputs: {
        pool_out: Array(WhitewaterParticle),
        state_out: Array(u32),
        counts_out: Array(u32),
        foam_particles: Array(FluidParticle),
        bubble_particles: Array(FluidParticle),
        spray_particles: Array(FluidParticle),
        dust_particles: Array(FluidParticle),
        foam_count: ScalarF32,
        bubble_count: ScalarF32,
        spray_count: ScalarF32,
        dust_count: ScalarF32,
        emitted: ScalarF32,
        thinned: ScalarF32,
        pool_full: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("enabled"),
            label: "Whitewater",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &["Off", "On"],
        },
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Whitewater Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("capacity"),
            label: "Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(DEFAULT_CAPACITY as f32),
            range: Some((1.0, MAX_CAPACITY as f32)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("wavecrest_emission"),
            label: "Wavecrest Emission",
            ty: ParamType::Float,
            default: ParamValue::Float(WAVECREST_RATE),
            range: Some((0.0, 1.0e5)),
            enum_values: &[],
        },
        ParamDef { name: Cow::Borrowed("turbulence_emission"), label: "Turbulence Emission", ty: ParamType::Float, default: ParamValue::Float(175.0), range: Some((0.0, 1.0e5)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("min_turbulence"), label: "Min Turbulence", ty: ParamType::Float, default: ParamValue::Float(100.0), range: Some((0.0, 1.0e5)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("max_turbulence"), label: "Max Turbulence", ty: ParamType::Float, default: ParamValue::Float(200.0), range: Some((0.0, 1.0e5)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("dust_emission"), label: "Dust Emission", ty: ParamType::Bool, default: ParamValue::Bool(false), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("boundary_dust"), label: "Boundary Dust", ty: ParamType::Bool, default: ParamValue::Bool(false), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("dust_rate"), label: "Dust Rate", ty: ParamType::Float, default: ParamValue::Float(175.0), range: Some((0.0, 1.0e5)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("influence_base"), label: "Base Influence", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0e5)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("influence_decay"), label: "Influence Decay", ty: ParamType::Float, default: ParamValue::Float(2.0), range: Some((0.0, 1.0e5)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("spray_speed"), label: "Spray Emission Speed", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((1.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("generation_rate"), label: "Emitter Generation", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("inside_emission"), label: "Inside Emission", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef {
            name: Cow::Borrowed("min_energy"),
            label: "Min Energy",
            ty: ParamType::Float,
            default: ParamValue::Float(MIN_ENERGY),
            range: Some((0.0, 1.0e4)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_energy"),
            label: "Max Energy",
            ty: ParamType::Float,
            default: ParamValue::Float(MAX_ENERGY),
            range: Some((0.0, 1.0e4)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("preserve_foam"),
            label: "Preserve Foam",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("pool_tiles", "Pool Tiles", 1.0, 1.0, 16_777_216.0),
        float_param!("bounds_lo_x", "Bounds Min X", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_lo_y", "Bounds Min Y", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_lo_z", "Bounds Min Z", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_hi_x", "Bounds Max X", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_hi_y", "Bounds Max Y", 0.0, -1.0e4, 1.0e4),
        float_param!("bounds_hi_z", "Bounds Max Z", 0.0, -1.0e4, 1.0e4),
        int_param!("open_faces", "Open Faces (bits −X +X −Y +Y −Z +Z)", 0.0, 0.0, 63.0),
        float_param!("kill_height", "Kill Height", f32::MIN, f32::MIN, f32::MAX),
    ],
    depth_rule: Terminal,
    composition_notes: "For per-tick GPU FLIP, wire the solver's faces, distance, tiles, pool_tiles and cell_size from node.gpu_flip_step, particles and identity (its identity_out, whose record counts the particles in use), solid and obstacle_source from node.whitewater_obstacle_source, and substep_schedule, substep_faces and substep_count from its history; bounds_lo_x/y/z are the Bounds' low corner, where the force fields start. Wire bounds_hi_x/y/z, open_faces and kill_height from the domain as the liquid step takes them: whitewater leaves where the step removes water, beyond the plane of an Open face or below the kill height. Wire pool and pool_state from liquid_state, and close pool_out, state_out, counts_out and the three population arrays through its capture results. tick_index is the boundary clock; each invocation emits and advances one tick. The boundary publishes capacity-sized populations with zero-radius unused records, so particles_to_copies needs no CPU live_count. Wire capacity to both the stage and boundary. Amount scales emission, while zero leaves the existing pool advancing; disabling clears the pool on the next tick.",
    examples: [],
    picker: { label: "Whitewater Step", category: Atom },
    summary: "Makes and moves the spray, foam and bubbles a liquid throws up, all on the GPU.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater", "foam", "spray", "bubbles", "diffuse particles", "gpu whitewater"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        step: Step = Step::default(),
    },
}

manifold_core::testkit_visible! {
/// The pool and capacity one frame's inputs describe, once every placement
/// rule held. A change in either starts the stage's storage over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StepShape {
    pub pool: TilePool,
    pub capacity: u32,
}
}

impl StepShape {
    /// Every placement rule, each a refusal by name: a capacity in range and
    /// a pool one dispatch can count.
    pub fn new(pool: TilePool, capacity: u32) -> Result<Self, String> {
        if !(1..=MAX_CAPACITY).contains(&capacity) {
            return Err(format!(
                "capacity {capacity} is outside 1 to {MAX_CAPACITY}"
            ));
        }
        if pool.elements() > u64::from(u32::MAX) {
            return Err(format!(
                "a pool of {} tiles has more elements than one dispatch counts",
                pool.pool_tiles
            ));
        }
        Ok(Self { pool, capacity })
    }

    /// Elements of every grid array: 512 a tile and the canonical one.
    pub fn elements(&self) -> u64 {
        self.pool.elements()
    }

    /// Each population output: a particle per pool slot.
    pub fn population_bytes(&self) -> u64 {
        u64::from(self.capacity) * PARTICLE
    }

    pub fn pool_bytes(&self) -> u64 {
        u64::from(self.capacity) * POOL_SLOT
    }

    pub fn spawn_bytes(&self) -> u64 {
        u64::from(self.capacity) * SPAWN
    }

    /// The pooled sort's ranges: one per element.
    pub fn range_bytes(&self) -> u64 {
        key_storage_bytes(self.elements())
    }

    /// The slot scan: keep flags over the pool, live flags over the spawns,
    /// population flags over three pools.
    pub fn slot_scan_values(&self) -> usize {
        4 * self.capacity as usize
    }

    /// The table's slot rows (a tile and an entry per slot), the part of the
    /// influence's owner the stage keeps between frames.
    pub fn slot_row_bytes(&self) -> u64 {
        u64::from(SLOT_WORDS) * 4 * u64::from(self.pool.pool_tiles)
    }

    /// Bytes the node holds for this shape and `particles` liquid particle
    /// slots, grid fields, scan, sort and output slots.
    pub fn held_bytes(&self, particles: u64) -> u64 {
        let elements = self.elements();
        let grid = elements * (4 + 2 * KNOWN_VALUE + 4 + 2 * 4 + SOURCE) + self.slot_row_bytes();
        let per_particle =
            particles * (2 * PARTICLE + 4 + 4 + 4) + storage_words(particles as usize) as u64 * 4;
        let capacity = u64::from(self.capacity);
        let pool = self.spawn_bytes() + 2 * self.pool_bytes() + capacity * 4;
        let sort =
            2 * capacity * 4 + storage_words(elements as usize) as u64 * 4 + self.range_bytes();
        let scan = storage_words(self.slot_scan_values()) as u64 * 4 + STATE_WORDS * 4;
        let outputs = 4 * self.population_bytes() + COUNT_WORDS as u64 * 4;
        grid + per_particle + pool + sort + scan + outputs + scratch_bytes(self.pool.pool_tiles)
    }
}

manifold_core::testkit_visible! {
/// Where whitewater leaves the scene, as the liquid step's move removes
/// water: beyond the plane of an Open face of the Bounds, or below the kill
/// height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Removal {
    /// The Open faces, bit 2d the low face of axis d and bit 2d + 1 the high.
    pub open_faces: u32,
    /// The Bounds' high corner, metres.
    pub bounds_hi: [f32; 3],
    /// Metres; f32::MIN with no floor to fall past.
    pub kill_height: f32,
}}

#[cfg(feature = "gpu-proofs")]
impl Removal {
    /// Nothing leaves: no Open face and no kill height.
    pub(crate) const NONE: Self = Self {
        open_faces: 0,
        bounds_hi: [0.0; 3],
        kill_height: f32::MIN,
    };
}

manifold_core::testkit_visible! {
/// One frame's scalar inputs and knobs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StepFrame {
    pub shape: StepShape,
    pub ticks: u32,
    /// Simulation seconds in the accepted interval: one Sim Rate interval of
    /// transport in export, every owed interval as one span live, both scaled
    /// by Speed.
    pub dt: f32,
    pub epoch: u32,
    pub seed: f32,
    pub gravity: [f32; 3],
    /// The Bounds' low corner, metres: where the force fields start.
    pub bounds_lo: [f32; 3],
    /// Where whitewater leaves the scene.
    pub removal: Removal,
    pub wavecrest_emission: f32,
    pub turbulence_emission: f32,
    pub min_turbulence: f32,
    pub max_turbulence: f32,
    pub inside_emission: bool,
    pub generation_rate: f32,
    pub spray_speed: f32,
    pub dust_emission: bool,
    pub boundary_dust: bool,
    pub dust_rate: f32,
    pub influence_base: f32,
    pub influence_decay: f32,
    pub min_energy: f32,
    pub max_energy: f32,
    pub preserve_foam: bool,
}
}

manifold_core::testkit_visible! {
pub(crate) struct MotionInputs<'a> {
    pub schedule: &'a GpuBuffer,
    /// The substeps' pooled faces, `count` rows of `pool_len` records of
    /// four floats, on the table the step publishes.
    pub faces: &'a GpuBuffer,
    pub count: u32,
    pub fields: ForceLatticeBinding<'a>,
    pub tick_index: f32,
    pub regions: Option<&'a GpuBuffer>,
    pub shapes: Option<&'a GpuBuffer>,
    pub atlas: Option<&'a GpuBuffer>,
    pub collider_nodes: Option<&'a GpuBuffer>,
    pub collider_triangles: Option<&'a GpuBuffer>,
    pub region_count: u32,
}
}

manifold_core::testkit_visible! {
pub(crate) struct StepInputs<'a> {
    pub motion: Option<MotionInputs<'a>>,
    pub particles: &'a GpuBuffer,
    /// The particle identity: words 4-6 dispatch a pass over the particles in
    /// use, word 7 counts them. Slots at or past it are never read.
    pub identity: &'a GpuBuffer,
    /// The solid distance, a pooled node array.
    pub solid: &'a GpuBuffer,
    pub obstacle_source: Option<&'a GpuBuffer>,
    /// The step's pooled faces (`pool_len` records), its pooled phi
    /// (`pool_len` floats) and its table, all of the frame's pool.
    pub faces: &'a GpuBuffer,
    pub distance: &'a GpuBuffer,
    pub tiles: &'a GpuBuffer,
}
}

/// Each input holds at least what the shape reads from it.
fn require_inputs(shape: &StepShape, inputs: &StepInputs<'_>) -> Result<(), String> {
    let elements = shape.elements();
    if let Some(m) = &inputs.motion {
        if m.schedule.size < u64::from(m.count) * 16 {
            return Err("substep schedule is too short".into());
        }
        if m.faces.size < u64::from(m.count) * elements * HISTORY_RECORD {
            return Err("substep face history is too short".into());
        }
        if m.region_count > 0 && (m.regions.is_none()
                || m.shapes.is_none()
                || m.atlas.is_none()
                || m.collider_nodes.is_none()
                || m.collider_triangles.is_none())
        {
            return Err("outflows require regions, shapes, atlas and the collider trees".into());
        }
    }
    if inputs
        .obstacle_source
        .is_some_and(|source| source.size < elements * SOURCE)
    {
        return Err("obstacle source is shorter than the pool's node array".to_owned());
    }
    if inputs.identity.size < IDENTITY_BYTES {
        return Err(format!(
            "the identity holds {} bytes, fewer than the {IDENTITY_BYTES} of its record",
            inputs.identity.size
        ));
    }
    let wanted = [
        ("solid", inputs.solid, elements * 4),
        ("liquid distance", inputs.distance, elements * 4),
        ("faces", inputs.faces, elements * FACE_RECORD),
        ("tiles", inputs.tiles, shape.pool.table_bytes()),
    ];
    for (name, buffer, bytes) in wanted {
        if buffer.size < bytes {
            return Err(format!(
                "{name} holds {} bytes, fewer than the {bytes} its pool reads",
                buffer.size
            ));
        }
    }
    Ok(())
}

/// A codegen atom's uniform: its params in order, each overridden by name or
/// at its default, its derived uniforms, then the dispatch count, padded to
/// 16 bytes.
struct Uniform {
    words: [u32; 64],
    len: usize,
}

fn pack<P: Primitive>(values: &[(&str, f32)], count: u32) -> Uniform {
    let params = P::PARAMS;
    debug_assert!(
        values
            .iter()
            .all(|(name, _)| params.iter().any(|p| p.name == *name)),
        "{} takes no param among {values:?}",
        P::TYPE_ID
    );
    let mut words = [0u32; 64];
    for (word, p) in words.iter_mut().zip(params) {
        let value = values
            .iter()
            .find(|(name, _)| p.name == *name)
            .map(|&(_, v)| v)
            .unwrap_or(match p.default {
                ParamValue::Float(v) => v,
                ParamValue::Bool(b) => f32::from(u8::from(b)),
                _ => 0.0,
            });
        *word = match p.ty {
            ParamType::Int => value.round() as i32 as u32,
            ParamType::Bool | ParamType::Enum => value.round().max(0.0) as u32,
            _ => value.to_bits(),
        };
    }
    // Derived uniforms sit between the params and the count, as the codegen
    // lays them out; one not named in `values` is 0, its unwired value.
    let mut at = params.len();
    for derived in P::DERIVED_UNIFORMS {
        let (name, ty) = derived.split_once(':').unwrap_or((derived, "f32"));
        let value = values
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(0.0, |&(_, v)| v);
        words[at] = match ty {
            "u32" => value.round().max(0.0) as u32,
            "f32" => value.to_bits(),
            other => panic!(
                "{}: the step packs scalar derived uniforms only, not {name}:{other}",
                P::TYPE_ID
            ),
        };
        at += 1;
    }
    words[at] = count;
    Uniform {
        words,
        len: (at + 1).next_multiple_of(4),
    }
}

/// Whether a dispatch ends with a barrier. `None` only when the next
/// dispatch reads nothing this one writes: the barrier the next one ends
/// with covers both.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Barrier {
    After,
    None,
}

/// How many groups a pass runs.
#[derive(Clone, Copy)]
enum Grid<'a> {
    /// One thread each over a count the CPU knows.
    Threads(u32),
    /// The triple at the identity's [`COUNT_OFFSET`], written on the GPU
    /// earlier in the command buffer.
    Particles(&'a GpuBuffer),
}

/// Dispatch `pipeline` over `count` threads, the uniform at 0 and `buffers`
/// from 1 in order, then `barrier`.
fn dispatch(
    enc: &mut manifold_gpu::GpuEncoder,
    pipeline: &GpuComputePipeline,
    uniform: &[u8],
    buffers: &[&GpuBuffer],
    count: u32,
    label: &str,
    barrier: Barrier,
) {
    if count == 0 {
        return;
    }
    encode(
        enc,
        pipeline,
        uniform,
        buffers,
        Grid::Threads(count),
        label,
        barrier,
    );
}

/// [`dispatch`] over the liquid particles in use, a thread each, the groups
/// read from `identity` on the GPU. An empty population still runs one group
/// that returns.
fn dispatch_particles(
    enc: &mut manifold_gpu::GpuEncoder,
    pipeline: &GpuComputePipeline,
    uniform: &[u8],
    buffers: &[&GpuBuffer],
    identity: &GpuBuffer,
    label: &str,
    barrier: Barrier,
) {
    encode(
        enc,
        pipeline,
        uniform,
        buffers,
        Grid::Particles(identity),
        label,
        barrier,
    );
}

fn encode(
    enc: &mut manifold_gpu::GpuEncoder,
    pipeline: &GpuComputePipeline,
    uniform: &[u8],
    buffers: &[&GpuBuffer],
    grid: Grid<'_>,
    label: &str,
    barrier: Barrier,
) {
    let bindings: [GpuBinding<'_>; 16] = std::array::from_fn(|i| match i {
        0 => GpuBinding::Bytes {
            binding: 0,
            data: uniform,
        },
        i if i <= buffers.len() => GpuBinding::Buffer {
            binding: i as u32,
            buffer: buffers[i - 1],
            offset: 0,
        },
        _ => GpuBinding::Bytes {
            binding: 0,
            data: uniform,
        },
    });
    let bindings = &bindings[..=buffers.len()];
    match grid {
        Grid::Threads(count) => {
            enc.dispatch_compute(pipeline, bindings, [count.div_ceil(256), 1, 1], label);
        }
        Grid::Particles(identity) => {
            enc.dispatch_compute_indirect(pipeline, bindings, identity, COUNT_OFFSET, label);
        }
    }
    if barrier == Barrier::After {
        enc.compute_memory_barrier_buffers();
    }
}

fn atom<P: Primitive>(
    enc: &mut manifold_gpu::GpuEncoder,
    pipeline: &GpuComputePipeline,
    values: &[(&str, f32)],
    buffers: &[&GpuBuffer],
    count: u32,
    label: &str,
) {
    atom_then::<P>(enc, pipeline, values, buffers, count, label, Barrier::After);
}

fn atom_then<P: Primitive>(
    enc: &mut manifold_gpu::GpuEncoder,
    pipeline: &GpuComputePipeline,
    values: &[(&str, f32)],
    buffers: &[&GpuBuffer],
    count: u32,
    label: &str,
    barrier: Barrier,
) {
    let uniform = pack::<P>(values, count);
    dispatch(
        enc,
        pipeline,
        bytemuck::cast_slice(&uniform.words[..uniform.len]),
        buffers,
        count,
        label,
        barrier,
    );
}

#[derive(Default)]
struct Pipelines {
    liquid: Option<GpuComputePipeline>,
    curvature: Option<GpuComputePipeline>,
    influence: Option<GpuComputePipeline>,
    extend: Option<GpuComputePipeline>,
    preserve: Option<GpuComputePipeline>,
    keep: Option<GpuComputePipeline>,
    /// `whitewater_step.wgsl`'s influence restart and its saving of the
    /// slot rows.
    restart: Option<GpuComputePipeline>,
    save_rows: Option<GpuComputePipeline>,
    /// `whitewater_step.wgsl`, in [`Hand`] order.
    hand: Vec<GpuComputePipeline>,
    /// `whitewater_fused.wgsl` in [`FUSED_ENTRIES`] order.
    fused: Vec<GpuComputePipeline>,
}

#[derive(Clone, Copy)]
enum Hand {
    SeedPool,
    LiveFlags,
    Append,
    AppendState,
    Compact,
    CompactState,
    SplitFlags,
    Split,
    PublishCounts,
}

const HAND_ENTRIES: [(&str, &str); 9] = [
    ("seed_pool", "node.whitewater_step.seed"),
    ("live_flags", "node.whitewater_step.live_flags"),
    ("append", "node.whitewater_step.append"),
    ("append_state", "node.whitewater_step.append_state"),
    ("compact", "node.whitewater_step.compact"),
    ("compact_state", "node.whitewater_step.compact_state"),
    ("split_flags", "node.whitewater_step.split_flags"),
    ("split", "node.whitewater_step.split"),
    ("publish_counts", "node.whitewater_step.counts"),
];

const SORT_LABELS: SortLabels = SortLabels {
    clear: "node.whitewater_step.sort.clear",
    count: "node.whitewater_step.sort.count",
    ranges: "node.whitewater_step.sort.ranges",
    tail: "node.whitewater_step.sort.tail",
    scatter: "node.whitewater_step.sort.scatter",
    stabilise: "node.whitewater_step.sort.stabilise",
    scan: ScanLabels {
        blocks: "node.whitewater_step.sort.scan.blocks",
        add: "node.whitewater_step.sort.scan.add",
    },
};

/// The four scans of a tick, told apart in a profile: how many each liquid
/// particle emits, the append of the spawns, the keep's compaction, and the
/// split into populations.
const EMISSION_SCAN: ScanLabels = ScanLabels {
    blocks: "node.whitewater_step.emission_scan.blocks",
    add: "node.whitewater_step.emission_scan.add",
};
const APPEND_SCAN: ScanLabels = ScanLabels {
    blocks: "node.whitewater_step.append_scan.blocks",
    add: "node.whitewater_step.append_scan.add",
};
const KEEP_SCAN: ScanLabels = ScanLabels {
    blocks: "node.whitewater_step.keep_scan.blocks",
    add: "node.whitewater_step.keep_scan.add",
};
const SPLIT_SCAN: ScanLabels = ScanLabels {
    blocks: "node.whitewater_step.split_scan.blocks",
    add: "node.whitewater_step.split_scan.add",
};

impl Pipelines {
    fn prepare(&mut self, device: &GpuDevice) {
        standalone_pipeline::<LiquidCells>(&mut self.liquid, device);
        standalone_pipeline::<WhitewaterInfluence>(&mut self.influence, device);
        standalone_pipeline::<LatticeCurvature>(&mut self.curvature, device);
        standalone_pipeline::<ExtendLattice>(&mut self.extend, device);
        standalone_pipeline::<PreserveFoam>(&mut self.preserve, device);
        standalone_pipeline::<KeepWhitewater>(&mut self.keep, device);
        self.restart.get_or_insert_with(|| {
            device.create_compute_pipeline(WHITEWATER_STEP_SHADER, RESTART_ENTRY.0, RESTART_ENTRY.1)
        });
        self.save_rows.get_or_insert_with(|| {
            device.create_compute_pipeline(
                WHITEWATER_STEP_SHADER,
                SAVE_ROWS_ENTRY.0,
                SAVE_ROWS_ENTRY.1,
            )
        });
        if self.fused.is_empty() {
            let source = fused_source();
            for (entry, label) in FUSED_ENTRIES {
                self.fused
                    .push(device.create_compute_pipeline(&source, entry, label));
            }
        }
        if self.hand.is_empty() {
            for (entry, label) in HAND_ENTRIES {
                self.hand.push(device.create_compute_pipeline(
                    WHITEWATER_STEP_SHADER,
                    entry,
                    label,
                ));
            }
        }
    }

    fn hand(&self, pass: Hand) -> (&GpuComputePipeline, &'static str) {
        (&self.hand[pass as usize], HAND_ENTRIES[pass as usize].1)
    }
}

fn get(slot: &Option<GpuComputePipeline>) -> &GpuComputePipeline {
    slot.as_ref().expect("whitewater step pipelines prepared")
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HandParams {
    capacity: u32,
    spawn_slots: u32,
    count: u32,
    _pad: u32,
}

fn allocate(device: &GpuDevice, bytes: u64, shared: bool) -> Result<GpuBuffer, String> {
    manifold_node_engine::load::expand::admit_world_bytes(
        device.modifier_memory_snapshot(),
        bytes,
    )
    .map_err(|error| error.to_string())?;
    let bytes = bytes.max(16);
    if shared {
        device.try_create_buffer(bytes, BufferUse::Upload)
    } else {
        device.try_create_buffer(bytes, BufferUse::Gpu)
    }
}

/// The grid fields and the pool, sized from one [`StepShape`]. Every grid
/// field holds one element per pool index.
struct Fields {
    cells: GpuBuffer,
    curvature: [GpuBuffer; 2],
    turbulence: GpuBuffer,
    influence: [GpuBuffer; 2],
    empty_source: GpuBuffer,
    /// Last frame's slot rows of the table (a tile and an entry per slot):
    /// the influence outlives the frame, a slot may not.
    slot_rows: GpuBuffer,
    typed: GpuBuffer,
    pools: [GpuBuffer; 2],
    order: GpuBuffer,
    state: GpuBuffer,
    /// The slot scan's storage, level 0 at offset 0.
    scan: GpuBuffer,
}

impl Fields {
    fn new(device: &GpuDevice, shape: &StepShape, scan: GpuBuffer) -> Result<Self, String> {
        let elements = shape.elements();
        let alloc = |bytes| allocate(device, bytes, false);
        Ok(Self {
            cells: alloc(elements * 4)?,
            curvature: [
                alloc(elements * KNOWN_VALUE)?,
                alloc(elements * KNOWN_VALUE)?,
            ],
            turbulence: alloc(elements * 4)?,
            influence: [alloc(elements * 4)?, alloc(elements * 4)?],
            empty_source: alloc(elements * SOURCE)?,
            slot_rows: alloc(shape.slot_row_bytes())?,
            typed: alloc(shape.spawn_bytes())?,
            pools: [alloc(shape.pool_bytes())?, alloc(shape.pool_bytes())?],
            order: alloc(u64::from(shape.capacity) * 4)?,
            state: alloc(STATE_WORDS * 4)?,
            scan,
        })
    }
}

/// Per liquid particle scratch, grown to the particle array. The dust
/// arrays exist only while dust emission is on.
#[derive(Default)]
struct ParticleScratch {
    slots: u32,
    /// sampled, energy (36 bytes per slot).
    core: Option<[GpuBuffer; 2]>,
    dust_slots: u32,
    /// unscaled, dust_energy, wavecrest_bits (40 bytes per slot).
    dust: Option<[GpuBuffer; 3]>,
    /// Stands in for a dust array no pass reads or writes.
    placeholder: Option<GpuBuffer>,
}

impl ParticleScratch {
    /// sampled, energy, unscaled, dust_energy, wavecrest_bits.
    fn reserve(
        &mut self,
        device: &GpuDevice,
        slots: u32,
        dust: bool,
    ) -> Result<[GpuBuffer; 5], String> {
        let alloc = |bytes| allocate(device, bytes, false);
        let n = u64::from(slots);
        if self.core.is_none() || self.slots < slots {
            self.core = Some([alloc(n * PARTICLE)?, alloc(n * 4)?]);
            self.slots = slots;
        }
        if dust {
            if self.dust.is_none() || self.dust_slots < slots {
                self.dust = Some([alloc(n * PARTICLE)?, alloc(n * 4)?, alloc(n * 4)?]);
                self.dust_slots = slots;
            }
        } else {
            self.dust = None;
        }
        if self.placeholder.is_none() {
            self.placeholder = Some(alloc(16)?);
        }
        let [sampled, energy] = self.core.as_ref().expect("particle scratch allocated");
        let placeholder = self.placeholder.as_ref().expect("placeholder allocated");
        let [unscaled, dust_energy, wavecrest_bits] = match &self.dust {
            Some([unscaled, dust_energy, wavecrest_bits]) => [unscaled, dust_energy, wavecrest_bits],
            None => [placeholder; 3],
        };
        Ok([
            sampled.clone(),
            energy.clone(),
            unscaled.clone(),
            dust_energy.clone(),
            wavecrest_bits.clone(),
        ])
    }
}

pub(crate) struct OutputSlot {
    pub buffers: [GpuBuffer; 4],
    counts: GpuBuffer,
}

/// The output slot the boundary's captures read.
#[derive(Default)]
pub(crate) struct Outputs {
    slots: Vec<OutputSlot>,
}

impl Outputs {
    /// Allocate the slot if there is none.
    fn ensure(&mut self, device: &GpuDevice, shape: &StepShape) -> Result<(), String> {
        if !self.slots.is_empty() {
            return Ok(());
        }
        let population = shape.population_bytes();
        let shared = |bytes| allocate(device, bytes, true);
        let counts = shared(COUNT_WORDS as u64 * 4)?;
        counts.zero_fill();
        // Past each population's count the buffer is zero, from here on: the
        // split zeroes only what the slot's previous publish filled beyond
        // the new count, so the slot's counts must describe its buffers
        // from the first publish.
        let buffers = [
            shared(population)?,
            shared(population)?,
            shared(population)?,
            shared(population)?,
        ];
        for buffer in &buffers {
            buffer.zero_fill();
        }
        self.slots.push(OutputSlot { buffers, counts });
        Ok(())
    }
}

struct IdlePool {
    pool: GpuBuffer,
    state: GpuBuffer,
}

manifold_core::testkit_visible! {
/// The node's GPU side: pipelines, the pool and its fields, the outputs.
#[derive(Default)]
pub(crate) struct Step {
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    reference: reference::Reference,
    pipelines: Pipelines,
    surface_distance: SurfaceDistance,
    shape: Option<StepShape>,
    fields: Option<Fields>,
    particles: ParticleScratch,
    /// The empty pool and zero state `pool_out` and `state_out` hand on while
    /// the node is off and holds no fields.
    idle: Option<IdlePool>,
    emission_scan: PrefixScan,
    slot_scan: PrefixScan,
    sort: ParticleSorter,
    /// Which of the two pool buffers holds the pool.
    current: usize,
    influence_epoch: Option<u32>,
    influence_current: usize,
    pub(super) outputs: Outputs,
}
}

impl Step {
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn set_reference_capture_for_test(&mut self, capture: bool) {
        self.reference.capture = capture;
    }
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn set_reference_enabled_for_test(&mut self, enabled: bool) {
        self.reference.enabled = enabled;
    }
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn particle_snapshots_for_test(&self) -> Option<&[GpuBuffer; 3]> {
        self.reference.particle_snapshots.as_ref()
    }
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn turbulence_dispatches_for_test(&self) -> u32 {
        self.reference.turbulence_dispatches.get()
    }

    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn turbulence_for_test(&self) -> &GpuBuffer {
        &self.fields().turbulence
    }

    manifold_core::testkit_visible! {
        pub(crate) fn reserve(&mut self, device: &GpuDevice, shape: StepShape, particles: u64) -> Result<(), String> {
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            self.reference.reserve(device, particles as u32, shape.capacity)?;
            self.surface_distance.reserve(device, shape.pool.pool_tiles)?;
            if self.shape != Some(shape) {
                self.shape = None;
                self.fields = None;
                self.influence_epoch = None;
                self.outputs = Outputs::default();
                let held = shape.held_bytes(particles);
                manifold_node_engine::load::expand::admit_world_bytes(device.modifier_memory_snapshot(), held)
                    .map_err(|error| format!("the pool and its scratch need {held} bytes: {error}"))?;
                let scan = self.slot_scan.buffer(device, shape.slot_scan_values())?.clone();
                self.fields = Some(Fields::new(device, &shape, scan)?);
                self.sort.reserve_keys(device, shape.elements())?;
                self.shape = Some(shape);
            }
            Ok(())
        }
    }

    manifold_core::testkit_visible! {
        /// One liquid tick, with all persistent pool words supplied by the
        /// boundary. No CPU readback or display-frame clock participates.
        pub(crate) fn advance_tick(
            &mut self,
            gpu: &mut GpuEncoder<'_>,
            frame: &StepFrame,
            inputs: &StepInputs<'_>,
            pool: &GpuBuffer,
            state: &GpuBuffer,
            enabled: bool,
        ) -> Result<(), String> {
            let shape = frame.shape;
            require_inputs(&shape, inputs)?;
            if frame.dust_emission && inputs.obstacle_source.is_none() { return Err("dust emission requires nearest-object obstacle_source".to_owned()); }
            if pool.size != shape.pool_bytes() || state.size < STATE_WORDS * 4 {
                return Err("the boundary whitewater pool capacity or state words do not match the step".into());
            }
            self.pipelines.prepare(gpu.device);
            self.surface_distance.prepare(gpu.device);
            self.emission_scan.prepare(gpu.device);
            self.slot_scan.prepare(gpu.device);
            self.sort.prepare_pooled(gpu.device);
            self.current = 0;
            if !enabled {
                return self.idle_tick(gpu, &shape);
            }
            self.idle = None;
            self.reserve(gpu.device, shape, inputs.particles.size / PARTICLE)?;
            self.outputs.ensure(gpu.device, &shape)?;
            let enc = &mut *gpu.native_enc;
            let f = self.fields();
            enc.copy_buffer_to_buffer(pool, &f.pools[0], shape.pool_bytes());
            enc.copy_buffer_to_buffer(state, &f.state, STATE_WORDS * 4);
            let slots = (inputs.particles.size / PARTICLE) as u32;
            let scratch = self.particles.reserve(gpu.device, slots, frame.dust_emission)?;
            let offsets = self.emission_scan.buffer(gpu.device, slots.max(1) as usize)?.clone();
            let surface = self.emit(enc, frame, inputs, &scratch, &offsets);
            self.tick(enc, gpu.device, frame, inputs, &surface)?;
            self.publish(enc, &shape, 0);
            Ok(())
        }
    }

    /// An off tick holds no fields and scratch and records no passes. The
    /// first one after the node was on drops them and the stale outputs;
    /// the empty pool and zero state are seeded once per shape.
    fn idle_tick(&mut self, gpu: &mut GpuEncoder<'_>, shape: &StepShape) -> Result<(), String> {
        let stale_outputs = self
            .outputs
            .slots
            .first()
            .is_some_and(|slot| slot.buffers[0].size != shape.population_bytes());
        if self.shape.is_some() || stale_outputs {
            self.shape = None;
            self.fields = None;
            self.influence_epoch = None;
            self.particles = ParticleScratch::default();
            self.outputs = Outputs::default();
        }
        self.outputs.ensure(gpu.device, shape)?;
        if self.idle.as_ref().is_none_or(|idle| idle.pool.size != shape.pool_bytes()) {
            let idle = IdlePool {
                pool: allocate(gpu.device, shape.pool_bytes(), false)?,
                state: allocate(gpu.device, STATE_WORDS * 4, false)?,
            };
            self.seed(&mut *gpu.native_enc, shape, &idle);
            self.idle = Some(idle);
        }
        Ok(())
    }

    manifold_core::testkit_visible! {
        pub(crate) fn tick_output(&self, port: &str) -> Option<&GpuBuffer> {
            match port {
                "pool_out" => self.fields.as_ref().map(|f| &f.pools[self.current])
                    .or(self.idle.as_ref().map(|idle| &idle.pool)),
                "state_out" => self.fields.as_ref().map(|f| &f.state)
                    .or(self.idle.as_ref().map(|idle| &idle.state)),
                "counts_out" => self.outputs.slots.first().map(|s| &s.counts),
                _ => OUTPUTS.iter().position(|&name| name == port)
                    .and_then(|i| self.outputs.slots.first().map(|s| &s.buffers[i])),
            }
        }
    }

    fn fields(&self) -> &Fields {
        self.fields.as_ref().expect("whitewater fields allocated")
    }

    /// A pass of `whitewater_step.wgsl` over `count` threads; `bound` fills
    /// bindings 1 to 12 in the shader's order.
    fn hand(
        &self,
        enc: &mut manifold_gpu::GpuEncoder,
        pass: Hand,
        params: HandParams,
        bound: [&GpuBuffer; 12],
    ) {
        let (pipeline, label) = self.pipelines.hand(pass);
        dispatch(
            enc,
            pipeline,
            bytemuck::bytes_of(&params),
            &bound,
            params.count,
            label,
            Barrier::After,
        );
    }

    /// Bindings for a hand pass: the pool pair, the slot scan, the typed
    /// spawns and the state, with `state` standing in for whatever the pass
    /// never touches.
    fn bound<'a>(
        &'a self,
        pool_in: &'a GpuBuffer,
        pool_out: &'a GpuBuffer,
        extra: HandBuffers<'a>,
    ) -> [&'a GpuBuffer; 12] {
        let f = self.fields();
        let filler = &f.state;
        let [foam, bubble, spray, dust] = extra.populations.unwrap_or([filler; 4]);
        [
            pool_in,
            pool_out,
            &f.scan,
            &f.typed,
            &f.state,
            extra.offsets.unwrap_or(filler),
            foam,
            bubble,
            spray,
            extra.counts.unwrap_or(filler),
            dust,
            extra.identity.unwrap_or(filler),
        ]
    }

    fn seed(&self, enc: &mut manifold_gpu::GpuEncoder, shape: &StepShape, idle: &IdlePool) {
        let params = HandParams {
            capacity: shape.capacity,
            spawn_slots: 0,
            count: shape.capacity.max(STATE_WORDS as u32),
            _pad: 0,
        };
        // Only the pool and the state are touched; the state fills the rest.
        let mut bound = [&idle.state; 12];
        bound[0] = &idle.pool;
        bound[1] = &idle.pool;
        self.hand(enc, Hand::SeedPool, params, bound);
    }

    /// Influence belongs to world tiles, but the table hands out slots: every
    /// element of a slot that holds a different tile than last frame, or was
    /// free, starts over at the base level. `influence` is the array the
    /// frame decays from; a free slot is never read, so is left as it is.
    fn restart_moved_influence(
        &self,
        enc: &mut manifold_gpu::GpuEncoder,
        frame: &StepFrame,
        inputs: &StepInputs<'_>,
        influence: &GpuBuffer,
    ) {
        let pool_tiles = frame.shape.pool.pool_tiles;
        let params = RestartParams {
            pool_tiles,
            _pad_h: 0,
            count: pool_tiles * 512,
            base_level: frame.influence_base,
        };
        enc.dispatch_compute(
            get(&self.pipelines.restart),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                GpuBinding::Buffer {
                    binding: 13,
                    buffer: inputs.tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 14,
                    buffer: &self.fields().slot_rows,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 15,
                    buffer: influence,
                    offset: 0,
                },
            ],
            [params.count.div_ceil(256), 1, 1],
            RESTART_ENTRY.1,
        );
        enc.compute_memory_barrier_buffers();
    }

    /// FLIP's emitter: the surface's distance and curvature from the level
    /// set, each liquid particle's energy and wavecrest potential, how many
    /// it emits, the spawns, their kinds, appended to the pool. The emitters
    /// are the liquid particles in use, a count the identity holds on the GPU.
    fn emit(
        &mut self,
        enc: &mut manifold_gpu::GpuEncoder,
        frame: &StepFrame,
        inputs: &StepInputs<'_>,
        scratch: &[GpuBuffer; 5],
        offsets: &GpuBuffer,
    ) -> GpuBuffer {
        let s = &frame.shape;
        let slots = (inputs.particles.size / PARTICLE) as u32;
        let reset_influence = self.influence_epoch != Some(frame.epoch);
        self.influence_epoch = Some(frame.epoch);
        let influence_previous = self.influence_current;
        let influence_next = 1 - influence_previous;
        let f = self.fields();
        let p = &self.pipelines;
        let (pool_tiles, h) = (s.pool.pool_tiles, s.pool.cell_size);
        let elements = s.elements() as u32;
        let pooled = [("cell_size", h), ("pool_tiles", pool_tiles as f32)];
        // Resolve once for emit and lifecycle; the next encode overwrites
        // SurfaceDistance's current storage, so this view is never history.
        let distance = inputs.distance;
        let surface = self
            .surface_distance
            .encode(enc, distance, inputs.tiles, pool_tiles, h);
        if !reset_influence {
            self.restart_moved_influence(enc, frame, inputs, &f.influence[influence_previous]);
        }
        atom::<WhitewaterInfluence>(
            enc,
            get(&p.influence),
            &[
                ("base_level", frame.influence_base),
                ("decay_rate", frame.influence_decay),
                ("dt", frame.dt),
                pooled[0],
                ("reset", f32::from(u8::from(reset_influence))),
                (
                    "source_present",
                    f32::from(u8::from(inputs.obstacle_source.is_some())),
                ),
            ],
            &[
                &f.influence[influence_previous],
                inputs.solid,
                inputs.obstacle_source.unwrap_or(&f.empty_source),
                &f.influence[influence_next],
            ],
            elements,
            "node.whitewater_step.influence",
        );
        let save = RestartParams {
            pool_tiles,
            _pad_h: 0,
            count: SLOT_WORDS * pool_tiles,
            base_level: 0.0,
        };
        enc.dispatch_compute(
            get(&self.pipelines.save_rows),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&save),
                },
                GpuBinding::Buffer {
                    binding: 13,
                    buffer: inputs.tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 16,
                    buffer: &f.slot_rows,
                    offset: 0,
                },
            ],
            [save.count.div_ceil(256), 1, 1],
            SAVE_ROWS_ENTRY.1,
        );
        // Liquid cells and curvature both read the distance; neither reads
        // the other, so one barrier after the pair.
        atom_then::<LiquidCells>(
            enc,
            get(&p.liquid),
            &pooled,
            &[distance, inputs.solid, inputs.tiles, &f.cells],
            elements,
            "node.whitewater_step.liquid_cells",
            Barrier::None,
        );
        atom::<LatticeCurvature>(
            enc,
            get(&p.curvature),
            &pooled,
            &[surface, inputs.tiles, &f.curvature[0]],
            elements,
            "node.whitewater_step.curvature",
        );
        let mut curvature = 0;
        for _ in 0..3 {
            atom::<ExtendLattice>(
                enc,
                get(&p.extend),
                &pooled,
                &[
                    &f.curvature[curvature],
                    inputs.tiles,
                    &f.curvature[1 - curvature],
                ],
                elements,
                "node.whitewater_step.extend_curvature",
            );
            curvature = 1 - curvature;
        }
        let [sampled, energy, unscaled, dust_energy, wavecrest_bits] = scratch;
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        let (reference, emitters) = (self.reference.enabled, self.reference.emitters(inputs));
        self.turbulence(enc, frame, inputs, distance);
        let fused = &p.fused;
        emitter_path!(
            reference,
            {
                self.reference.emit_reference(
                    enc,
                    frame,
                    inputs,
                    f,
                    surface,
                    curvature,
                    influence_next,
                    offsets,
                    emitters,
                );
            },
            {
                let pass = Fused::Emit;
                dispatch_particles(
                    enc,
                    &fused[pass.index()],
                    bytemuck::bytes_of(&EmitParams::new(frame, false)),
                    &[
                        inputs.tiles,
                        inputs.particles,
                        inputs.faces,
                        surface,
                        &f.cells,
                        &f.curvature[curvature],
                        &f.turbulence,
                        &f.influence[influence_next],
                        inputs.identity,
                        sampled,
                        energy,
                        offsets,
                        unscaled,
                        wavecrest_bits,
                    ],
                    inputs.identity,
                    FUSED_ENTRIES[pass.index()].1,
                    Barrier::After,
                );
                #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
                self.reference
                    .record_dispatch(FUSED_ENTRIES[pass.index()].1, slots);
            }
        );
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        let (sampled, energy, unscaled) = if reference {
            let [jittered, sampled, energy, _, _] = self.reference.scratch();
            (jittered, energy, sampled)
        } else {
            (sampled, energy, unscaled)
        };
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        self.reference.capture_emit(
            enc,
            sampled,
            unscaled,
            energy,
            offsets,
            emitters,
            frame.dust_emission,
        );
        emitter_path!(
            reference,
            {
                self.emission_scan
                    .encode_labelled(enc, emitters.max(1) as usize, EMISSION_SCAN);
            },
            {
                self.emission_scan.encode_labelled_counted(
                    enc,
                    slots.max(1) as usize,
                    EMISSION_SCAN,
                    inputs.identity,
                    COUNT_OFFSET,
                );
            }
        );
        self.spawn(enc, frame, inputs, surface, offsets, sampled, energy, false);
        let params = HandParams {
            capacity: s.capacity,
            spawn_slots: s.capacity,
            count: s.capacity,
            _pad: 0,
        };
        let pool = &f.pools[self.current];
        self.hand(
            enc,
            Hand::LiveFlags,
            params,
            self.bound(pool, pool, HandBuffers::default()),
        );
        self.slot_scan
            .encode_labelled(enc, s.capacity as usize, APPEND_SCAN);
        self.hand(
            enc,
            Hand::Append,
            params,
            self.bound(pool, pool, HandBuffers::default()),
        );
        let state = HandParams { count: 1, ..params };
        self.hand(
            enc,
            Hand::AppendState,
            state,
            self.bound(
                pool,
                pool,
                HandBuffers {
                    offsets: Some(offsets),
                    identity: Some(inputs.identity),
                    ..HandBuffers::default()
                },
            ),
        );
        if frame.dust_emission {
            emitter_path!(
                reference,
                {
                    self.reference.dust_reference(
                        enc,
                        frame,
                        inputs,
                        f,
                        influence_next,
                        offsets,
                        emitters,
                    );
                },
                {
                    let pass = Fused::Dust;
                    // Only the dust arrays are read here; the filler stands in
                    // for the bindings this pass never touches.
                    let filler = &f.state;
                    dispatch_particles(
                        enc,
                        &fused[pass.index()],
                        bytemuck::bytes_of(&EmitParams::new(frame, true)),
                        &[
                            inputs.tiles,
                            unscaled,
                            inputs.obstacle_source.expect("validated dust source"),
                            filler,
                            filler,
                            inputs.solid,
                            &f.turbulence,
                            &f.influence[influence_next],
                            inputs.identity,
                            filler,
                            dust_energy,
                            offsets,
                            filler,
                            wavecrest_bits,
                        ],
                        inputs.identity,
                        FUSED_ENTRIES[pass.index()].1,
                        Barrier::After,
                    );
                    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
                    self.reference
                        .record_dispatch(FUSED_ENTRIES[pass.index()].1, slots);
                }
            );
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            let dust_energy = if reference {
                self.reference.dust_energy()
            } else {
                dust_energy
            };
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            self.reference
                .capture_dust(enc, dust_energy, offsets, emitters);
            emitter_path!(
                reference,
                {
                    self.emission_scan.encode_labelled(
                        enc,
                        emitters.max(1) as usize,
                        EMISSION_SCAN,
                    );
                },
                {
                    self.emission_scan.encode_labelled_counted(
                        enc,
                        slots.max(1) as usize,
                        EMISSION_SCAN,
                        inputs.identity,
                        COUNT_OFFSET,
                    );
                }
            );
            self.spawn(
                enc,
                frame,
                inputs,
                surface,
                offsets,
                unscaled,
                dust_energy,
                true,
            );
            self.hand(
                enc,
                Hand::LiveFlags,
                params,
                self.bound(pool, pool, HandBuffers::default()),
            );
            self.slot_scan
                .encode_labelled(enc, s.capacity as usize, APPEND_SCAN);
            self.hand(
                enc,
                Hand::Append,
                params,
                self.bound(pool, pool, HandBuffers::default()),
            );
            self.hand(
                enc,
                Hand::AppendState,
                state,
                self.bound(
                    pool,
                    pool,
                    HandBuffers {
                        offsets: Some(offsets),
                        identity: Some(inputs.identity),
                        ..HandBuffers::default()
                    },
                ),
            );
        }
        let surface = surface.clone();
        self.influence_current = influence_next;
        surface
    }

    fn turbulence(
        &self,
        enc: &mut manifold_gpu::GpuEncoder,
        frame: &StepFrame,
        inputs: &StepInputs<'_>,
        distance: &GpuBuffer,
    ) {
        let f = self.fields();
        emitter_path!(
            self.reference.enabled,
            {
                self.reference
                    .turbulence_reference(enc, frame, inputs, distance, &f.turbulence);
            },
            {
                let s = &frame.shape;
                let params = TurbulenceParams {
                    cell_size: s.pool.cell_size,
                    pool_tiles: s.pool.pool_tiles as f32,
                    count: s.elements() as u32,
                    _pad0: 0,
                };
                let pass = Fused::Turbulence;
                dispatch(
                    enc,
                    &self.pipelines.fused[pass.index()],
                    bytemuck::bytes_of(&params),
                    &[inputs.tiles, &f.turbulence, inputs.faces, distance],
                    params.count,
                    FUSED_ENTRIES[pass.index()].1,
                    Barrier::After,
                );
                #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
                self.reference
                    .record_dispatch(FUSED_ENTRIES[pass.index()].1, params.count);
            }
        );
    }

    /// Spawn and classify before append, preserving the atom defaults for dust typing.
    fn spawn(
        &self,
        enc: &mut manifold_gpu::GpuEncoder,
        frame: &StepFrame,
        inputs: &StepInputs<'_>,
        surface: &GpuBuffer,
        offsets: &GpuBuffer,
        sampled: &GpuBuffer,
        energy: &GpuBuffer,
        dust: bool,
    ) {
        let f = self.fields();
        emitter_path!(
            self.reference.enabled,
            {
                let emitters = self.reference.emitters(inputs);
                self.reference.spawn_reference(
                    enc, frame, inputs, f, surface, offsets, sampled, energy, emitters, dust,
                );
            },
            {
                let pass = Fused::Spawn;
                let label = if dust {
                    "node.whitewater_step.dust_spawn"
                } else {
                    FUSED_ENTRIES[pass.index()].1
                };
                dispatch(
                    enc,
                    &self.pipelines.fused[pass.index()],
                    bytemuck::bytes_of(&SpawnParams::new(frame, dust)),
                    &[
                        inputs.tiles,
                        sampled,
                        inputs.faces,
                        surface,
                        &f.cells,
                        inputs.solid,
                        offsets,
                        energy,
                        inputs.identity,
                        &f.typed,
                    ],
                    frame.shape.capacity,
                    label,
                    Barrier::After,
                );
                #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
                self.reference.record_dispatch(label, frame.shape.capacity);
            }
        );
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        self.reference
            .capture_particle(enc, &f.typed, usize::from(dust));
    }

    /// Advect, retype and age `a` into `b`, in the fused kernel and through
    /// the atoms alike. Sort and all current/preserve/compaction bookkeeping
    /// stay in tick.
    fn lifecycle(
        &self,
        enc: &mut manifold_gpu::GpuEncoder,
        frame: &StepFrame,
        inputs: &StepInputs<'_>,
        surface: &GpuBuffer,
        a: &GpuBuffer,
        b: &GpuBuffer,
    ) {
        let f = self.fields();
        emitter_path!(
            self.reference.enabled,
            {
                self.reference
                    .lifecycle_reference(enc, frame, inputs, f, surface, a, b);
            },
            {
                let motion = inputs.motion.as_ref();
                let empty = &f.state;
                let pass = Fused::Lifecycle;
                dispatch(
                    enc,
                    &self.pipelines.fused[pass.index()],
                    bytemuck::bytes_of(&LifecycleParams::new(frame, inputs)),
                    &[
                        inputs.tiles,
                        a,
                        inputs.faces,
                        surface,
                        &f.cells,
                        inputs.solid,
                        motion.map_or(empty, |m| m.schedule),
                        motion.map_or(empty, |m| m.faces),
                        motion.and_then(|m| m.fields.forces).unwrap_or(empty),
                        motion.and_then(|m| m.fields.impulses).unwrap_or(empty),
                        b,
                    ],
                    frame.shape.capacity,
                    FUSED_ENTRIES[pass.index()].1,
                    Barrier::After,
                );
                #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
                self.reference
                    .record_dispatch(FUSED_ENTRIES[pass.index()].1, frame.shape.capacity);
            }
        );
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        self.reference.capture_particle(enc, b, 2);
    }

    /// One tick of FLIP's whitewater: advect, retype, age, sort, preserve
    /// foam, mark the slots to keep, compact.
    fn tick(
        &mut self,
        enc: &mut manifold_gpu::GpuEncoder,
        device: &GpuDevice,
        frame: &StepFrame,
        inputs: &StepInputs<'_>,
        surface: &GpuBuffer,
    ) -> Result<(), String> {
        let s = &frame.shape;
        let f = self
            .fields
            .as_ref()
            .expect("the step's fields are allocated");
        let p = &self.pipelines;
        let cap = s.capacity;
        let (a, b) = (&f.pools[self.current], &f.pools[1 - self.current]);
        let (pool_tiles, h) = (s.pool.pool_tiles, s.pool.cell_size);
        let pooled = [("cell_size", h), ("pool_tiles", pool_tiles as f32)];
        let dt = frame.dt;
        self.lifecycle(enc, frame, inputs, surface, a, b);
        let motion = inputs.motion.as_ref();
        let empty = &f.state;
        let job = SortJob {
            particles: b,
            read: whitewater_record_read(),
            capacity: cap,
            count: SortCount::Slots(cap),
            bin_min: lattice_origin(h),
            inv_cell: 1.0 / h,
            bins: [0; 3],
            sorted: None,
            order: Some(&f.order),
            companion: None,
            gate: None,
        };
        self.sort
            .encode_pooled(device, enc, &job, inputs.tiles, pool_tiles, &SORT_LABELS)?;
        let ranges = self.sort.ranges().expect("ranges reserved with the shape");
        // Preserve foam off is the identity (FLIP skips the pass,
        // diffuseparticlesimulation.cpp:2124), so the aged pool `b` goes
        // straight to the keep and the compaction lands back in `a`; on, the
        // preserved pool is `a` and the compaction lands in `b`. Either way
        // `current` ends on the compacted pool.
        let (stepped, out) = if frame.preserve_foam {
            atom::<PreserveFoam>(
                enc,
                get(&p.preserve),
                &[("enabled", 1.0), ("dt", dt), pooled[0], pooled[1]],
                &[b, b, ranges, &f.order, inputs.tiles, a],
                cap,
                "node.whitewater_step.preserve_foam",
            );
            (a, b)
        } else {
            (b, a)
        };
        let keep = [
            (
                "region_count",
                motion.map_or(0.0, |m| m.region_count as f32),
            ),
            (
                "region_offset",
                motion.map_or(0.0, |m| {
                    (m.tick_index - m.fields.first_tick as f32).max(0.0) * m.region_count as f32
                }),
            ),
            ("tick_seconds", frame.dt),
            pooled[0],
            pooled[1],
            ("open_faces", frame.removal.open_faces as f32),
            ("bounds_lo_x", frame.bounds_lo[0]),
            ("bounds_lo_y", frame.bounds_lo[1]),
            ("bounds_lo_z", frame.bounds_lo[2]),
            ("bounds_hi_x", frame.removal.bounds_hi[0]),
            ("bounds_hi_y", frame.removal.bounds_hi[1]),
            ("bounds_hi_z", frame.removal.bounds_hi[2]),
            ("kill_height", frame.removal.kill_height),
        ];
        atom::<KeepWhitewater>(
            enc,
            get(&p.keep),
            &keep,
            &[
                stepped,
                stepped,
                ranges,
                &f.order,
                inputs.solid,
                motion.and_then(|m| m.regions).unwrap_or(empty),
                motion.and_then(|m| m.shapes).unwrap_or(empty),
                motion.and_then(|m| m.atlas).unwrap_or(empty),
                motion.and_then(|m| m.collider_nodes).unwrap_or(empty),
                motion.and_then(|m| m.collider_triangles).unwrap_or(empty),
                inputs.tiles,
                &f.scan,
            ],
            cap,
            "node.whitewater_step.keep",
        );
        self.slot_scan.encode_labelled(enc, cap as usize, KEEP_SCAN);
        let params = HandParams {
            capacity: cap,
            spawn_slots: 0,
            count: cap,
            _pad: 0,
        };
        self.hand(
            enc,
            Hand::Compact,
            params,
            self.bound(stepped, out, HandBuffers::default()),
        );
        self.hand(
            enc,
            Hand::CompactState,
            HandParams { count: 1, ..params },
            self.bound(stepped, out, HandBuffers::default()),
        );
        if frame.preserve_foam {
            self.current = 1 - self.current;
        }
        Ok(())
    }

    /// Split the pool into foam, bubbles and spray in output slot `index`,
    /// and its counts beside them.
    fn publish(&self, enc: &mut manifold_gpu::GpuEncoder, shape: &StepShape, index: usize) {
        let f = self.fields();
        let slot = &self.outputs.slots[index];
        let pool = &f.pools[self.current];
        let [foam, bubble, spray, dust] = &slot.buffers;
        let extra = || HandBuffers {
            populations: Some([foam, bubble, spray, dust]),
            counts: Some(&slot.counts),
            ..HandBuffers::default()
        };
        let params = HandParams {
            capacity: shape.capacity,
            spawn_slots: 0,
            count: 4 * shape.capacity,
            _pad: 0,
        };
        self.hand(
            enc,
            Hand::SplitFlags,
            params,
            self.bound(pool, pool, extra()),
        );
        self.slot_scan
            .encode_labelled(enc, shape.slot_scan_values(), SPLIT_SCAN);
        self.hand(enc, Hand::Split, params, self.bound(pool, pool, extra()));
        self.hand(
            enc,
            Hand::PublishCounts,
            HandParams { count: 1, ..params },
            self.bound(pool, pool, extra()),
        );
    }
}

/// Buffers a hand pass binds beyond the step's own.
#[derive(Default)]
struct HandBuffers<'a> {
    offsets: Option<&'a GpuBuffer>,
    /// The particle identity, whose word 7 counts the emitters.
    identity: Option<&'a GpuBuffer>,
    populations: Option<[&'a GpuBuffer; 4]>,
    counts: Option<&'a GpuBuffer>,
}

impl WhitewaterStep {
    fn frame(ctx: &EffectNodeContext<'_, '_>, pool: TilePool) -> Result<StepFrame, String> {
        let whole = |v: f32| v.round().max(0.0) as u32;
        let capacity = ctx
            .scalar_or_param("capacity", DEFAULT_CAPACITY as f32)
            .round();
        if !(1.0..=MAX_CAPACITY as f32).contains(&capacity) {
            return Err(format!(
                "capacity {capacity} is outside 1 to {MAX_CAPACITY}"
            ));
        }
        let amount = ctx.param_f32("amount", 1.0);
        if !amount.is_finite() || amount < 0.0 {
            return Err(format!("Whitewater Amount {amount} must be 0 or more"));
        }
        let shape = StepShape::new(pool, capacity as u32)?;
        let bounds_lo = ["bounds_lo_x", "bounds_lo_y", "bounds_lo_z"]
            .map(|name| ctx.scalar_or_param(name, 0.0));
        if bounds_lo.iter().any(|v| !v.is_finite()) {
            return Err("the Bounds' low corner must be finite".to_owned());
        }
        let bounds_hi = ["bounds_hi_x", "bounds_hi_y", "bounds_hi_z"]
            .map(|name| ctx.scalar_or_param(name, 0.0));
        if bounds_hi.iter().any(|v| !v.is_finite()) {
            return Err("the Bounds' high corner must be finite".to_owned());
        }
        let open_faces = ctx.scalar_or_param("open_faces", 0.0);
        if !(open_faces.fract() == 0.0 && (0.0..=63.0).contains(&open_faces)) {
            return Err(format!(
                "Open Faces must be a whole number from 0 to 63, not {open_faces}"
            ));
        }
        let kill_height = ctx.scalar_or_param("kill_height", f32::MIN);
        if kill_height.is_nan() {
            return Err("the kill height must be a number".to_owned());
        }
        let (min_t, max_t) = (
            ctx.param_f32("min_turbulence", 100.0),
            ctx.param_f32("max_turbulence", 200.0),
        );
        if !min_t.is_finite() || !max_t.is_finite() || min_t < 0.0 || max_t <= min_t {
            return Err(
                "Whitewater: Max Turbulence must exceed nonnegative Min Turbulence".to_owned(),
            );
        }
        Ok(StepFrame {
            // A graph saved without the domain's interval wire runs on the project's Sim Rate.
            dt: ctx.scalar_or_param("dt", ctx.sim_step.interval.0 as f32),
            shape,
            ticks: 1,
            epoch: whole(ctx.scalar_or_param("epoch", 0.0)),
            seed: ctx.scalar_or_param("tick_index", 0.0) * TICK as f32,
            gravity: [
                ctx.scalar_or_param("gravity_x", 0.0),
                ctx.scalar_or_param("gravity", -9.81),
                ctx.scalar_or_param("gravity_z", 0.0),
            ],
            bounds_lo,
            removal: Removal {
                open_faces: open_faces as u32,
                bounds_hi,
                kill_height,
            },
            wavecrest_emission: ctx.param_f32("wavecrest_emission", WAVECREST_RATE) * amount,
            dust_emission: matches!(
                ctx.params.get("dust_emission"),
                Some(ParamValue::Bool(true))
            ),
            boundary_dust: matches!(
                ctx.params.get("boundary_dust"),
                Some(ParamValue::Bool(true))
            ),
            dust_rate: ctx.param_f32("dust_rate", 175.0) * amount,
            influence_base: ctx.param_f32("influence_base", 1.0),
            influence_decay: ctx.param_f32("influence_decay", 2.0),
            spray_speed: ctx.param_f32("spray_speed", 1.0),
            generation_rate: ctx.param_f32("generation_rate", 1.0),
            turbulence_emission: ctx.param_f32("turbulence_emission", 175.0) * amount,
            min_turbulence: ctx.param_f32("min_turbulence", 100.0),
            max_turbulence: ctx.param_f32("max_turbulence", 200.0),
            inside_emission: !matches!(
                ctx.params.get("inside_emission"),
                Some(ParamValue::Bool(false))
            ),
            min_energy: ctx.param_f32("min_energy", MIN_ENERGY),
            max_energy: ctx.param_f32("max_energy", MAX_ENERGY),
            preserve_foam: matches!(
                ctx.params.get("preserve_foam"),
                Some(ParamValue::Bool(true))
            ),
        })
    }
}

impl Primitive for WhitewaterStep {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.step.pipelines.prepare(device);
        self.step.surface_distance.prepare(device);
        self.step.emission_scan.prepare(device);
        self.step.slot_scan.prepare(device);
        self.step.sort.prepare_pooled(device);
    }

    fn provides_array_output(&self, port: &str) -> bool {
        OUTPUTS.contains(&port) || matches!(port, "pool_out" | "state_out" | "counts_out")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        if port == "proof_turbulence" {
            assert!(
                self.step.reference.capture && self.step.reference.turbulence_dispatches.get() > 0
            );
            return self.step.fields.as_ref().map(|f| &f.turbulence);
        }
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        if let Some(buffer) = self.step.reference.output(port) {
            return Some(buffer);
        }
        self.step.tick_output(port)
    }

    /// Each population output holds the whole pool.
    fn array_output_capacity(
        &self,
        port: &str,
        params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        if port == "counts_out" {
            return Some(COUNT_WORDS as u32);
        }
        if port == "state_out" {
            return Some(STATE_WORDS as u32);
        }
        (OUTPUTS.contains(&port) || port == "pool_out").then(|| match params.get("capacity") {
            Some(ParamValue::Float(v)) => (v.round().max(1.0) as u32).min(MAX_CAPACITY),
            _ => DEFAULT_CAPACITY,
        })
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let pool = match tile_pool(ctx, "Whitewater Step") {
            Ok(pool) => pool,
            Err(refusal) => {
                ctx.error(refusal);
                return;
            }
        };
        let frame = match Self::frame(ctx, pool) {
            Ok(frame) => frame,
            Err(refusal) => {
                ctx.error(format!("Whitewater Step: {refusal}"));
                return;
            }
        };
        let (
            Some(particles),
            Some(identity),
            Some(solid),
            Some(faces),
            Some(distance),
            Some(tiles),
        ) = (
            ctx.inputs.array("particles"),
            ctx.inputs.array("identity"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("faces"),
            ctx.inputs.array("distance"),
            ctx.inputs.array("tiles"),
        )
        else {
            ctx.error(
                "Whitewater Step: particles, identity, solid, faces, distance and tiles must all be wired",
            );
            return;
        };
        let motion = if let Some(schedule) = ctx.inputs.array("substep_schedule") {
            let Some(history) = ctx.inputs.array("substep_faces") else {
                ctx.error("Whitewater Step: an accepted schedule requires the substep faces");
                return;
            };
            let fields = match ForceLatticeBinding::read(
                ctx,
                ctx.inputs.array("forces"),
                ctx.inputs.array("impulses"),
                "Whitewater Step",
            ) {
                Ok(fields) => fields,
                Err(error) => {
                    ctx.error(error);
                    return;
                }
            };
            Some(MotionInputs {
                schedule,
                faces: history,
                count: ctx.scalar_or_param("substep_count", 0.0) as u32,
                fields,
                tick_index: ctx.scalar_or_param("tick_index", 0.0),
                regions: ctx.inputs.array("regions"),
                shapes: ctx.inputs.array("shapes"),
                atlas: ctx.inputs.array("atlas"),
                collider_nodes: ctx.inputs.array("collider_nodes"),
                collider_triangles: ctx.inputs.array("collider_triangles"),
                region_count: ctx.scalar_or_param("region_count", 0.0) as u32,
            })
        } else {
            None
        };
        let inputs = StepInputs {
            motion,
            particles,
            identity,
            solid,
            obstacle_source: ctx.inputs.array("obstacle_source"),
            faces,
            distance,
            tiles,
        };
        let (Some(pool), Some(state)) = (ctx.inputs.array("pool"), ctx.inputs.array("pool_state"))
        else {
            ctx.error("Whitewater Step: per-tick distance requires pool and pool_state from the liquid boundary");
            return;
        };
        let enabled = ctx.param_f32("enabled", 1.0) >= 0.5;
        if let Err(error) =
            self.step
                .advance_tick(ctx.gpu_encoder(), &frame, &inputs, pool, state, enabled)
        {
            ctx.error(format!("Whitewater Step: {error}"));
        }
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod copy_tests;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(any(test, feature = "testkit"))]
pub fn empty_slot() -> WhitewaterParticle {
    WhitewaterParticle {
        kind: manifold_water_liquid::whitewater::WHITEWATER_EMPTY,
        ..Default::default()
    }
}
