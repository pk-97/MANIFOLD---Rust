//! `node.whitewater_step` — FLIP whitewater emission and lifecycle.
//! GPU FLIP runs this stage once per liquid tick using solver particle phi.
//! The liquid boundary captures the pool, IDs, counters and rendering arrays
//! after each tick; the stage owns reusable scratch. The legacy level-set
//! interface retains its frame publication ring for existing saved graphs.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_water_liquid::fluid_particles::WhitewaterSpawn;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use super::crossing_distance::CrossingDistance;
use super::emission_count::WAVECREST_RATE;
use super::whitewater_influence::WhitewaterInfluence;
use super::whitewater_obstacle_source::WhitewaterSource;
use super::energy_potential::{MAX_ENERGY, MIN_ENERGY};
use super::extend_lattice::ExtendLattice;
use super::keep_whitewater::KeepWhitewater;
use super::lattice_curvature::LatticeCurvature;
use manifold_water_liquid::primitives::liquid_cells::LiquidCells;
use super::nearest_crossing::NearestCrossing;
use super::pad_distance_lattice::{encode_pad_distance_lattice, prepare_pad_distance_lattice};
use manifold_water_liquid::primitives::prefix_scan::{PrefixScan, ScanLabels, storage_words};
use super::preserve_foam::PreserveFoam;
use manifold_water_liquid::primitives::sort_particles_into_cells::{ParticleSorter, SortJob, SortLabels, range_storage_bytes, whitewater_record_read};
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use super::surface_crossings::SurfaceCrossings;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_physics::clock::TICK;
use manifold_node_engine::particles::FluidParticle;
use manifold_water_liquid::fluid_particles::{FaceSample, MAX_BINS, bin_counts, bin_total};
use manifold_water_liquid::grid::face_len;
use manifold_water_liquid::bodies::{LiquidBody, LiquidShape};
use manifold_water_liquid::fields::FieldBinding;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::transform::Transform;
use manifold_water_liquid::whitewater::{DEFAULT_CAPACITY, MAX_CAPACITY, KnownValue, SPREAD_STEPS, SURFACE_CROSSING_BYTES, WhitewaterParticle, cell_total, face_offset, grid_box, grid_cells, refinement, require_extended_faces};
use crate::whitewater_handoff::{Fence, Retired};


const WHITEWATER_FUSED_SHADER: &str = include_str!("shaders/whitewater_fused.wgsl");
const FUSED_ENTRIES: [(&str, &str); 6] = [
    ("ww_emit", "node.whitewater_step.emit"),
    ("ww_dust", "node.whitewater_step.dust"),
    ("ww_spawn", "node.whitewater_step.spawn"),
    ("ww_lifecycle", "node.whitewater_step.lifecycle"),
    ("ww_turbulence", "node.whitewater_step.turbulence"),
    ("ww_unpack_faces", "node.whitewater_step.unpack_faces"),
];

#[derive(Clone, Copy)]
enum Fused {
    Emit,
    Dust,
    Spawn,
    Lifecycle,
    Turbulence,
    UnpackFaces,
}

impl Fused {
    const fn index(self) -> usize { self as usize }
}

fn fused_source() -> String {
    use manifold_water_liquid::whitewater::WHITEWATER_COMMON;
    use manifold_water_liquid::{grid::LIQUID_FACES, fields::LIQUID_FIELD};
    format!("{WHITEWATER_COMMON}\n{LIQUID_FACES}\n{LIQUID_FIELD}\n{WHITEWATER_FUSED_SHADER}")
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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UnpackParams {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TurbulenceParams {
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EmitParams {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    cell_size: f32,
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
    live_count: f32,
    dt: f32,
    dust_enabled: f32,
    boundary_dust: f32,
    count: u32,
    _pad: [u32; 2],
}

impl EmitParams {
    fn new(frame: &StepFrame, count: u32, dust: bool) -> Self {
        let s = &frame.shape;
        let [center_x, center_y, center_z] = s.center;
        let [size_x, size_y, size_z] = s.size;
        let [nodes_x, nodes_y, nodes_z] = s.nodes.map(|n| n as f32);
        let [face_cells_x, face_cells_y, face_cells_z] = s.face_cells.map(|n| n as f32);
        Self {
            center_x, center_y, center_z, size_x, size_y, size_z,
            nodes_x, nodes_y, nodes_z, face_cells_x, face_cells_y, face_cells_z,
            cell_size: s.cell_size, seed: if dust { frame.seed + 104729.0 } else { frame.seed },
            epoch: frame.epoch as f32, spray_speed: frame.spray_speed,
            min_energy: frame.min_energy, max_energy: frame.max_energy,
            min_curvature: super::wavecrest_potential::MIN_CURVATURE,
            max_curvature: super::wavecrest_potential::MAX_CURVATURE,
            sharpness: super::wavecrest_potential::SHARPNESS,
            min_turbulence: frame.min_turbulence, max_turbulence: frame.max_turbulence,
            inside_enabled: f32::from(u8::from(frame.inside_emission)),
            rate: if dust { 0.0 } else { frame.wavecrest_emission },
            turbulence_rate: if dust { frame.dust_rate } else { frame.turbulence_emission },
            generation_rate: frame.generation_rate, points_per_cell: 8.0,
            ticks: frame.ticks as f32, live_count: count as f32, dt: frame.dt,
            dust_enabled: f32::from(u8::from(frame.dust_emission)),
            boundary_dust: f32::from(u8::from(frame.boundary_dust)), count, _pad: [0; 2],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpawnParams {
    capacity: f32,
    emitters: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
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
    count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LifecycleParams {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
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
    bubble_lifetime_modifier: f32,
    foam_lifetime_modifier: f32,
    spray_lifetime_modifier: f32,
    count: u32,
    _pad0: u32,
}

impl SpawnParams {
    fn new(frame: &StepFrame, emitters: u32, dust: bool) -> Self {
        let s = &frame.shape;
        let [center_x, center_y, center_z] = s.center;
        let [size_x, size_y, size_z] = s.size;
        let [nodes_x, nodes_y, nodes_z] = s.nodes.map(|n| n as f32);
        let [face_cells_x, face_cells_y, face_cells_z] = s.face_cells.map(|n| n as f32);
        Self {
            capacity: s.capacity as f32, emitters: emitters as f32,
            center_x, center_y, center_z, size_x, size_y, size_z,
            nodes_x, nodes_y, nodes_z, face_cells_x, face_cells_y, face_cells_z,
            seed: if dust { frame.seed + 104729.0 } else { frame.seed }, epoch: frame.epoch as f32,
            min_lifetime: super::spawn_whitewater::MIN_LIFETIME,
            max_lifetime: super::spawn_whitewater::MAX_LIFETIME,
            lifetime_variance: super::spawn_whitewater::LIFETIME_VARIANCE,
            dt: frame.dt, spray_speed: if dust { 1.0 } else { frame.spray_speed },
            type_seed: if dust { 0.0 } else { frame.seed }, type_epoch: if dust { 0.0 } else { frame.epoch as f32 },
            dust: f32::from(u8::from(dust)), count: s.capacity, _pad0: 0, _pad1: 0, _pad2: 0,
        }
    }
}

impl LifecycleParams {
    fn new(frame: &StepFrame, inputs: &StepInputs<'_>) -> Self {
        use super::{advect_whitewater as advect, age_whitewater as age};
        let s = &frame.shape;
        let [center_x, center_y, center_z] = s.center;
        let [size_x, size_y, size_z] = s.size;
        let [nodes_x, nodes_y, nodes_z] = s.nodes.map(|n| n as f32);
        let [face_cells_x, face_cells_y, face_cells_z] = s.face_cells.map(|n| n as f32);
        let [gravity_x, gravity_y, gravity_z] = frame.gravity;
        let motion = inputs.motion.as_ref();
        let [field_nodes_x, field_nodes_y, field_nodes_z] = motion.map_or([2; 3], |m| m.fields.nodes).map(|n| n as f32);
        Self {
            center_x, center_y, center_z, size_x, size_y, size_z,
            nodes_x, nodes_y, nodes_z, face_cells_x, face_cells_y, face_cells_z,
            gravity_x, gravity_y, gravity_z, dt: frame.dt,
            foam_advection: advect::FOAM_ADVECTION, bubble_buoyancy: advect::BUBBLE_BUOYANCY, bubble_drag: advect::BUBBLE_DRAG,
            spray_drag: advect::SPRAY_DRAG, spray_drag_variance: advect::SPRAY_DRAG_VARIANCE,
            spray_restitution: advect::SPRAY_RESTITUTION, spray_friction: advect::SPRAY_FRICTION,
            substep_count: motion.map_or(0.0, |m| m.count as f32),
            field_nodes_x, field_nodes_y, field_nodes_z,
            field_spacing: motion.map_or(0.25, |m| m.fields.spacing),
            force_lattices: motion.map_or(0.0, |m| m.fields.force_lattices as f32),
            tick_index: motion.map_or(0.0, |m| m.tick_index), first_tick: motion.map_or(0.0, |m| m.fields.first_tick as f32),
            bubble_lifetime_modifier: age::BUBBLE_LIFETIME_MODIFIER, foam_lifetime_modifier: age::FOAM_LIFETIME_MODIFIER,
            spray_lifetime_modifier: age::SPRAY_LIFETIME_MODIFIER,
            count: s.capacity, _pad0: 0,
        }
    }
}

manifold_core::testkit_visible! {
pub(crate) const WHITEWATER_STEP_SHADER: &str = include_str!("shaders/whitewater_step.wgsl");
}

pub(crate) const OUTPUTS: [&str; 4] = ["foam_particles", "bubble_particles", "spray_particles", "dust_particles"];
/// The population counts, in [`OUTPUTS`]' order.
const OUTPUT_COUNTS: [&str; 3] = ["foam_count", "bubble_count", "spray_count"];

/// Published outputs in flight or on show; four cover live's three frames
/// behind and the one being written.
pub(crate) const OUTPUT_SLOTS: usize = 4;

/// Words the GPU writes per output slot: foam, bubble and spray counts,
/// emitted, thinned, pool full, live, next id, dust count (first eight preserved).
const COUNT_WORDS: usize = 9;
const STATE_WORDS: u64 = 8;

const PARTICLE: u64 = std::mem::size_of::<FluidParticle>() as u64;
const POOL_SLOT: u64 = std::mem::size_of::<WhitewaterParticle>() as u64;
const SPAWN: u64 = std::mem::size_of::<WhitewaterSpawn>() as u64;
const KNOWN_VALUE: u64 = std::mem::size_of::<KnownValue>() as u64;

manifold_node_engine::primitive! {
    name: WhitewaterStep,
    type_id: "node.whitewater_step",
    purpose: "With distance and pool state wired from a liquid tick region, emit and advance exactly one tick and publish through captured boundary results. The legacy level-set interface provides foam, bubbles and spray for a GPU liquid, all on the GPU as FLIP's whitewater: each frame with ticks, emit from where the surface crests and the water moves fast, then step the pool once per tick (spray falls and bounces, bubbles rise and drag, foam rides the surface, each ages, dies, and leaves when it strays or crowds its cell). Out come foam, bubbles and spray as particle frames, each particle's radius its fade, with their counts. The outputs trail the water by one frame offline and up to three live. A new epoch, grid or capacity clears the pool; ticks 0 holds it. Spawns past the pool's room are counted as pool full; emissions past Capacity in one frame are thinned evenly and counted.",
    inputs: {
        particles: Array(FluidParticle) required,
        count: ScalarF32 optional,
        capacity: ScalarF32 optional,
        solid: Array(f32) required,
        obstacle_source: Array(WhitewaterSource) optional,
        grid_bounds: Transform optional,
        grid_nodes_x: ScalarF32 optional, grid_nodes_y: ScalarF32 optional, grid_nodes_z: ScalarF32 optional,
        faces: Array(FaceSample) optional,
        face_u: Array(f32) optional, face_v: Array(f32) optional, face_w: Array(f32) optional,
        face_cells_x: ScalarF32 optional, face_cells_y: ScalarF32 optional, face_cells_z: ScalarF32 optional,
        face_valid_layers: ScalarF32 optional,
        level_set: Array(f32) optional,
        distance: Array(f32) optional,
        substep_schedule: Array(f32) optional,
        substep_u: Array(f32) optional, substep_v: Array(f32) optional, substep_w: Array(f32) optional,
        substep_count: ScalarF32 optional,
        forces: Array(f32) optional, impulses: Array(f32) optional,
        field_nodes_x: ScalarF32 optional, field_nodes_y: ScalarF32 optional, field_nodes_z: ScalarF32 optional,
        field_spacing: ScalarF32 optional, force_lattices: ScalarF32 optional,
        impulse_tick: ScalarF32 optional, first_tick: ScalarF32 optional,
        regions: Array(LiquidBody) optional, shapes: Array(LiquidShape) optional, atlas: Array(u32) optional,
        region_count: ScalarF32 optional,
        pool: Array(WhitewaterParticle) optional,
        pool_state: Array(u32) optional,
        tick_index: ScalarF32 optional,
        level_set_nodes_x: ScalarF32 optional, level_set_nodes_y: ScalarF32 optional, level_set_nodes_z: ScalarF32 optional,
        ticks: ScalarF32 optional,
        dt: ScalarF32 optional,
        epoch: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        seed: ScalarF32 optional,
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
    ],
    depth_rule: Terminal,
    composition_notes: "For per-tick GPU FLIP, wire the solver distance, particles and packed faces (or all three projected face components); wire pool and pool_state from liquid_state, and close pool_out, state_out, counts_out and the three population arrays through its capture results. tick_index is the boundary clock; each invocation emits and advances one tick. The boundary publishes capacity-sized populations with zero-radius unused records, so particles_to_copies needs no CPU live_count. Wire capacity to both the stage and boundary. The legacy level_set interface keeps its frame ticks, seed, scalar counts and retired output ring for saved graphs. Amount scales emission, while zero leaves the existing pool advancing; disabling clears the pool on the next tick.",
    examples: [],
    picker: { label: "Whitewater Step", category: Atom },
    summary: "Makes and moves the spray, foam and bubbles a liquid throws up, all on the GPU.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater", "foam", "spray", "bubbles", "diffuse particles", "gpu whitewater"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        step: Step = Step::default(),
        tick_mode: bool = false,
    },
}

manifold_core::testkit_visible! {
/// The grid and pool one frame's inputs describe, once every placement rule
/// held. A change in any of it starts the pool over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StepShape {
    pub nodes: [u32; 3],
    pub cells: [u32; 3],
    pub level_nodes: [u32; 3],
    pub face_cells: [u32; 3],
    pub center: [f32; 3],
    pub size: [f32; 3],
    pub cell_size: f32,
    /// The pool sort's bins: one a cell.
    pub bins: [u32; 3],
    pub capacity: u32,
}
}

impl StepShape {
    /// Every placement rule, each a refusal by name: a lattice with cube
    /// cells, a level set refining it whole, a face grid centred on it and
    /// extended at least a layer, a capacity in range.
    pub fn new(
        nodes: [u32; 3],
        level_nodes: [u32; 3],
        face_cells: [u32; 3],
        face_valid_layers: f32,
        bounds: Option<Transform>,
        capacity: u32,
    ) -> Result<Self, String> {
        if nodes == [0; 3] {
            return Err("the solid lattice is missing: grid_nodes_x/y/z are 0".into());
        }
        let cells = grid_cells(nodes).ok_or_else(|| format!("a {nodes:?} solid lattice has too few or too many nodes"))?;
        let bounds = bounds.ok_or("the solid lattice is missing: grid_bounds is not wired")?;
        let (origin, cell_size) = grid_box(bounds, nodes)?;
        refinement(nodes, level_nodes)?;
        face_offset(nodes, face_cells)?;
        require_extended_faces(face_valid_layers)?;
        if !(1..=MAX_CAPACITY).contains(&capacity) {
            return Err(format!("capacity {capacity} is outside 1 to {MAX_CAPACITY}"));
        }
        let size: [f32; 3] = std::array::from_fn(|a| cells[a] as f32 * cell_size);
        let center: [f32; 3] = std::array::from_fn(|a| origin[a] + 0.5 * size[a]);
        let bins = bin_counts(size, cell_size);
        if bin_total(bins) > MAX_BINS {
            return Err(format!("a {bins:?} sort grid is more than the {MAX_BINS} bins a search can index"));
        }
        Ok(Self { nodes, cells, level_nodes, face_cells, center, size, cell_size, bins, capacity })
    }

    pub fn cell_count(&self) -> u64 {
        cell_total(self.cells)
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

    pub fn range_bytes(&self) -> u64 {
        range_storage_bytes(self.bins)
    }

    /// The slot scan: keep flags over the pool, live flags over the spawns,
    /// population flags over three pools.
    pub fn slot_scan_values(&self) -> usize {
        4 * self.capacity as usize
    }

    pub fn face_bytes(&self, axis: usize) -> u64 {
        face_len(self.face_cells, axis) * 4
    }

    pub fn solid_bytes(&self) -> u64 {
        cell_total(self.nodes) * 4
    }

    pub fn level_bytes(&self) -> u64 {
        cell_total(self.level_nodes) * 4
    }

    /// Bytes the node holds for this shape and `particles` liquid particle
    /// slots, axis-input scratch, scan, sort and output slots. Packed face
    /// storage is added separately by unpacked_face_bytes. Tick mode at pad
    /// zero borrows distance fields instead of allocating them.
    pub fn held_bytes(&self, particles: u64, tick_mode: bool) -> u64 {
        let cells = self.cell_count();
        let distance_fields = if self.owns_distance_fields(tick_mode) { 8 } else { 0 };
        let grid = cells * (2 * SURFACE_CROSSING_BYTES + distance_fields + 4 + 2 * KNOWN_VALUE + 4);
        let per_particle = particles * (2 * PARTICLE + 4 + 4 + 4) + storage_words(particles as usize) as u64 * 4;
        let capacity = u64::from(self.capacity);
        let pool = self.spawn_bytes() + 2 * self.pool_bytes() + capacity * 4;
        let sort = 2 * capacity * 4 + storage_words(bin_total(self.bins) as usize) as u64 * 4 + self.range_bytes();
        let scan = storage_words(self.slot_scan_values()) as u64 * 4 + STATE_WORDS * 4;
        let outputs = OUTPUT_SLOTS as u64 * (4 * self.population_bytes() + COUNT_WORDS as u64 * 4);
        grid + per_particle + pool + sort + scan + outputs + 6 * self.solid_bytes()
            + manifold_water_liquid::primitives::whitewater_distance::scratch_bytes(self.face_cells)
    }

    /// Three adapter-sized arrays, including their zero tails, for packed input.
    pub fn unpacked_face_bytes(&self) -> u64 {
        3 * cell_total(self.face_cells.map(|n| n + 1)) * 4
    }

    fn owns_distance_fields(&self, tick_mode: bool) -> bool {
        !tick_mode || face_offset(self.nodes, self.face_cells).expect("validated face placement")[0] > 0
    }
}

manifold_core::testkit_visible! {
/// One frame's scalar inputs and knobs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StepFrame {
    pub shape: StepShape,
    /// Live liquid particles; `None` takes every slot.
    pub count: Option<u32>,
    pub ticks: u32,
    /// Simulation seconds in the accepted interval: one Sim Rate interval of
    /// transport in export, every owed interval as one span live, both scaled
    /// by Speed.
    pub dt: f32,
    pub epoch: u32,
    pub seed: f32,
    pub gravity: [f32; 3],
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
    pub faces: [&'a GpuBuffer; 3],
    pub count: u32,
    pub fields: FieldBinding<'a>,
    pub tick_index: f32,
    pub regions: Option<&'a GpuBuffer>,
    pub shapes: Option<&'a GpuBuffer>,
    pub atlas: Option<&'a GpuBuffer>,
    pub region_count: u32,
}
}

manifold_core::testkit_visible! {
#[derive(Clone, Copy)]
pub(crate) enum FaceSource<'a> {
    Packed(&'a GpuBuffer),
    Axes([&'a GpuBuffer; 3]),
}
}

/// Shared runtime/extent truth table. Packed input is unpacked before use;
/// every floating-point kernel reads axes. Legacy saved graphs keep their axis interface.
pub(crate) fn packed_face_source(tick: bool, packed: bool, axes: [bool; 3]) -> Result<bool, &'static str> {
    if packed && axes.iter().any(|&wired| wired) {
        return Err("Whitewater Step: wire faces, or face_u, face_v and face_w, not both");
    }
    if packed {
        return if tick { Ok(true) } else { Err("Whitewater Step: legacy level-set interface requires face_u, face_v and face_w; packed faces are not supported") };
    }
    match axes.into_iter().filter(|&wired| wired).count() {
        3 => Ok(false),
        0 => Err("Whitewater Step: wire faces, or face_u, face_v and face_w; neither source is wired"),
        _ => Err("Whitewater Step: partial axes; wire all of face_u, face_v and face_w"),
    }
}

impl<'a> FaceSource<'a> {
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    fn axes(self) -> [&'a GpuBuffer; 3] {
        match self {
            Self::Axes(axes) => axes,
            Self::Packed(_) => panic!("the atom oracle requires independently wired axis adapters"),
        }
    }
}

manifold_core::testkit_visible! {
pub(crate) struct StepInputs<'a> {
    pub motion: Option<MotionInputs<'a>>,
    pub particles: &'a GpuBuffer,
    pub solid: &'a GpuBuffer,
    pub obstacle_source: Option<&'a GpuBuffer>,
    pub faces: FaceSource<'a>,
    pub level_set: &'a GpuBuffer,
    pub distance: Option<&'a GpuBuffer>,
}
}

/// Each input holds at least what the shape reads from it.
fn require_inputs(shape: &StepShape, inputs: &StepInputs<'_>) -> Result<(), String> {
    if let Some(m) = &inputs.motion {
        if m.schedule.size < u64::from(m.count) * 16 { return Err("substep schedule is too short".into()); }
        for axis in 0..3 {
            if m.faces[axis].size < u64::from(m.count) * shape.face_bytes(axis) { return Err("substep face history is too short".into()); }
        }
        if m.region_count > 0 && (m.regions.is_none() || m.shapes.is_none() || m.atlas.is_none()) { return Err("outflows require regions, shapes and atlas".into()); }
    }
    if inputs.obstacle_source.is_some_and(|source| source.size < shape.solid_bytes() * 4) {
        return Err("obstacle source is shorter than the solid lattice".to_owned());
    }
    let wanted = [
        ("solid", inputs.solid, shape.solid_bytes()),
        ("liquid distance", inputs.distance.unwrap_or(inputs.level_set), if inputs.distance.is_some() { cell_total(shape.face_cells) * 4 } else { shape.level_bytes() }),
    ];
    for (name, buffer, bytes) in wanted {
        if buffer.size < bytes {
            return Err(format!("{name} holds {} bytes, fewer than the {bytes} its grid reads", buffer.size));
        }
    }
    match inputs.faces {
        FaceSource::Packed(faces) => {
            let bytes = manifold_water_liquid::grid::face_bytes(shape.face_cells);
            if inputs.distance.is_none() {
                return Err("legacy level-set interface requires axis arrays".into());
            }
            if faces.size < bytes { return Err(format!("faces holds {} bytes, fewer than the {bytes} its grid reads", faces.size)); }
        }
        FaceSource::Axes(axes) => {
            for (axis, buffer) in axes.into_iter().enumerate() {
                let name = ["face_u", "face_v", "face_w"][axis];
                let bytes = shape.face_bytes(axis);
                if buffer.size < bytes { return Err(format!("{name} holds {} bytes, fewer than the {bytes} its grid reads", buffer.size)); }
            }
        }
    }
    Ok(())
}

/// What the published outputs say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    /// Foam, bubbles, spray.
    pub counts: [u32; 3],
    pub dust: u32,
    pub emitted: u32,
    pub thinned: u32,
    pub pool_full: u32,
    /// Pool slots live and the next id, as the published frame left them.
    pub live: u32,
    pub next_id: u32,
}

impl Report {
    fn from_words(words: [u32; COUNT_WORDS]) -> Self {
        Self {
            counts: [words[0], words[1], words[2]],
            dust: words[8],
            emitted: words[3],
            thinned: words[4],
            pool_full: words[5],
            live: words[6],
            next_id: words[7],
        }
    }
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
        values.iter().all(|(name, _)| params.iter().any(|p| p.name == *name)),
        "{} takes no param among {values:?}",
        P::TYPE_ID
    );
    let mut words = [0u32; 64];
    for (word, p) in words.iter_mut().zip(params) {
        let value = values.iter().find(|(name, _)| p.name == *name).map(|&(_, v)| v).unwrap_or(match p.default {
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
        let value = values.iter().find(|(n, _)| *n == name).map_or(0.0, |&(_, v)| v);
        words[at] = match ty {
            "u32" => value.round().max(0.0) as u32,
            "f32" => value.to_bits(),
            other => panic!("{}: the step packs scalar derived uniforms only, not {name}:{other}", P::TYPE_ID),
        };
        at += 1;
    }
    words[at] = count;
    Uniform { words, len: (at + 1).next_multiple_of(4) }
}

/// Whether a dispatch ends with a barrier. `None` only when the next
/// dispatch reads nothing this one writes: the barrier the next one ends
/// with covers both.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Barrier {
    After,
    None,
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
    let bindings: [GpuBinding<'_>; 16] = std::array::from_fn(|i| match i {
        0 => GpuBinding::Bytes { binding: 0, data: uniform },
        i if i <= buffers.len() => GpuBinding::Buffer { binding: i as u32, buffer: buffers[i - 1], offset: 0 },
        _ => GpuBinding::Bytes { binding: 0, data: uniform },
    });
    enc.dispatch_compute(pipeline, &bindings[..=buffers.len()], [count.div_ceil(256), 1, 1], label);
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
    dispatch(enc, pipeline, bytemuck::cast_slice(&uniform.words[..uniform.len]), buffers, count, label, barrier);
}

#[derive(Default)]
struct Pipelines {
    pad_distance: Option<GpuComputePipeline>,
    crossings: Option<GpuComputePipeline>,
    nearest: Option<GpuComputePipeline>,
    distance: Option<GpuComputePipeline>,
    liquid: Option<GpuComputePipeline>,
    curvature: Option<GpuComputePipeline>,
    influence: Option<GpuComputePipeline>,
    extend: Option<GpuComputePipeline>,
    preserve: Option<GpuComputePipeline>,
    keep: Option<GpuComputePipeline>,
    /// `whitewater_step.wgsl`, in [`Hand`] order.
    hand: Vec<GpuComputePipeline>,
    /// `whitewater_fused.wgsl` in [`FUSED_ENTRIES`] order, including the face unpack pass.
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
    scan: ScanLabels { blocks: "node.whitewater_step.sort.scan.blocks", add: "node.whitewater_step.sort.scan.add" },
};

/// The four scans of a tick, told apart in a profile: how many each liquid
/// particle emits, the append of the spawns, the keep's compaction, and the
/// split into populations.
const EMISSION_SCAN: ScanLabels =
    ScanLabels { blocks: "node.whitewater_step.emission_scan.blocks", add: "node.whitewater_step.emission_scan.add" };
const APPEND_SCAN: ScanLabels =
    ScanLabels { blocks: "node.whitewater_step.append_scan.blocks", add: "node.whitewater_step.append_scan.add" };
const KEEP_SCAN: ScanLabels = ScanLabels { blocks: "node.whitewater_step.keep_scan.blocks", add: "node.whitewater_step.keep_scan.add" };
const SPLIT_SCAN: ScanLabels =
    ScanLabels { blocks: "node.whitewater_step.split_scan.blocks", add: "node.whitewater_step.split_scan.add" };

impl Pipelines {
    fn prepare(&mut self, device: &GpuDevice) {
        prepare_pad_distance_lattice(&mut self.pad_distance, device);
        standalone_pipeline::<SurfaceCrossings>(&mut self.crossings, device);
        standalone_pipeline::<NearestCrossing>(&mut self.nearest, device);
        standalone_pipeline::<CrossingDistance>(&mut self.distance, device);
        standalone_pipeline::<LiquidCells>(&mut self.liquid, device);
        standalone_pipeline::<WhitewaterInfluence>(&mut self.influence, device);
        standalone_pipeline::<LatticeCurvature>(&mut self.curvature, device);
        standalone_pipeline::<ExtendLattice>(&mut self.extend, device);
        standalone_pipeline::<PreserveFoam>(&mut self.preserve, device);
        standalone_pipeline::<KeepWhitewater>(&mut self.keep, device);
        if self.fused.is_empty() {
            let source = fused_source();
            for (entry, label) in FUSED_ENTRIES {
                self.fused.push(device.create_compute_pipeline(&source, entry, label));
            }
        }
        if self.hand.is_empty() {
            for (entry, label) in HAND_ENTRIES {
                self.hand.push(device.create_compute_pipeline(WHITEWATER_STEP_SHADER, entry, label));
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
    emitters: u32,
    count: u32,
}

fn allocate(device: &GpuDevice, bytes: u64, shared: bool) -> Result<GpuBuffer, String> {
    manifold_node_engine::load::expand::admit_candidate_bytes(device.modifier_memory_snapshot(), bytes)
        .map_err(|error| error.to_string())?;
    let bytes = bytes.max(16);
    if shared { device.try_create_buffer_shared(bytes) } else { device.try_create_buffer(bytes) }
}

/// The grid fields and the pool, sized from one [`StepShape`].
struct Fields {
    unpacked_faces: Option<[GpuBuffer; 3]>,
    crossings: [GpuBuffer; 2],
    distance: Option<GpuBuffer>,
    surface: Option<GpuBuffer>,
    cells: GpuBuffer,
    curvature: [GpuBuffer; 2],
    turbulence: GpuBuffer,
    influence: [GpuBuffer; 2],
    empty_source: GpuBuffer,
    typed: GpuBuffer,
    pools: [GpuBuffer; 2],
    order: GpuBuffer,
    state: GpuBuffer,
    /// The slot scan's storage, level 0 at offset 0.
    scan: GpuBuffer,
}

impl Fields {
    fn new(device: &GpuDevice, shape: &StepShape, tick_mode: bool, scan: GpuBuffer) -> Result<Self, String> {
        let cells = shape.cell_count();
        let alloc = |bytes| allocate(device, bytes, false);
        Ok(Self {
            unpacked_faces: None,
            crossings: [alloc(cells * SURFACE_CROSSING_BYTES)?, alloc(cells * SURFACE_CROSSING_BYTES)?],
            distance: shape.owns_distance_fields(tick_mode).then(|| alloc(cells * 4)).transpose()?,
            surface: shape.owns_distance_fields(tick_mode).then(|| alloc(cells * 4)).transpose()?,
            cells: alloc(cells * 4)?,
            curvature: [alloc(cells * KNOWN_VALUE)?, alloc(cells * KNOWN_VALUE)?],
            turbulence: alloc(cells * 4)?,
            influence: [alloc(shape.solid_bytes())?, alloc(shape.solid_bytes())?],
            empty_source: alloc(shape.solid_bytes() * 4)?,
            typed: alloc(shape.spawn_bytes())?,
            pools: [alloc(shape.pool_bytes())?, alloc(shape.pool_bytes())?],
            order: alloc(u64::from(shape.capacity) * 4)?,
            state: alloc(STATE_WORDS * 4)?,
            scan,
        })
    }
}

/// Per liquid particle scratch, grown to the particle array.
#[derive(Default)]
struct ParticleScratch {
    slots: u32,
    /// sampled, energy, unscaled, dust_energy, wavecrest_bits (76 bytes per slot).
    buffers: Option<[GpuBuffer; 5]>,
}

impl ParticleScratch {
    fn reserve(&mut self, device: &GpuDevice, slots: u32) -> Result<&[GpuBuffer; 5], String> {
        if self.buffers.is_none() || self.slots < slots {
            let n = u64::from(slots);
            let alloc = |bytes| allocate(device, bytes, false);
            self.buffers = Some([alloc(n * PARTICLE)?, alloc(n * 4)?, alloc(n * PARTICLE)?, alloc(n * 4)?, alloc(n * 4)?]);
            self.slots = slots;
        }
        Ok(self.buffers.as_ref().expect("particle scratch allocated"))
    }
}

pub(crate) struct OutputSlot {
    pub buffers: [GpuBuffer; 4],
    counts: GpuBuffer,
    /// The stamp of the frame that wrote it, until it is published or
    /// overtaken.
    written: Option<u64>,
    /// The stamp of the last frame that read it while published.
    read: u64,
}

/// Output slots: written on the GPU, published once that frame retired.
#[derive(Default)]
pub(crate) struct Outputs {
    slots: Vec<OutputSlot>,
    published: Option<(usize, u64)>,
    report: Report,
}

impl Outputs {
    fn clear(&mut self) {
        for slot in &mut self.slots {
            slot.written = None;
        }
        self.published = None;
        self.report = Report::default();
    }

    /// Publish the newest written slot whose frame retired; offline waits
    /// for it.
    fn collect(&mut self, fence: &dyn Fence, offline: bool) -> Result<(), String> {
        let newest = self.slots.iter().enumerate().filter_map(|(i, slot)| slot.written.map(|stamp| (i, stamp))).max_by_key(|&(_, stamp)| stamp);
        let Some((index, stamp)) = newest else { return Ok(()) };
        if offline && !fence.is_complete(stamp) && !fence.wait(stamp) {
            return Err("the GPU did not finish the frame that wrote the whitewater".into());
        }
        let ready = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.written.filter(|&s| fence.is_complete(s)).map(|s| (i, s)))
            .max_by_key(|&(_, s)| s);
        let Some((index, stamp)) = (if offline { Some((index, stamp)) } else { ready }) else { return Ok(()) };
        let mapped = self.slots[index].counts.mapped_ptr().ok_or("the whitewater counts are not readable")?;
        // SAFETY: a shared buffer of COUNT_WORDS words; the frame that wrote it retired.
        let words = unsafe { std::ptr::read(mapped.cast::<[u32; COUNT_WORDS]>()) };
        self.report = Report::from_words(words);
        self.published = Some((index, stamp));
        for slot in &mut self.slots {
            if slot.written.is_some_and(|s| s <= stamp) {
                slot.written = None;
            }
        }
        Ok(())
    }

    fn mark_read(&mut self, fence: &dyn Fence) {
        if let Some((index, _)) = self.published {
            self.slots[index].read = fence.stamp();
        }
    }

    /// A slot no frame still reads or writes, else a new one while there are
    /// fewer than [`OUTPUT_SLOTS`].
    fn free(&mut self, device: &GpuDevice, fence: &dyn Fence, shape: &StepShape) -> Result<Option<usize>, String> {
        let published = self.published.map(|(index, _)| index);
        let free = (0..self.slots.len())
            .find(|&i| Some(i) != published && self.slots[i].written.is_none() && fence.is_complete(self.slots[i].read));
        if free.is_some() || self.slots.len() >= OUTPUT_SLOTS {
            return Ok(free);
        }
        let population = shape.population_bytes();
        let shared = |bytes| allocate(device, bytes, true);
        let counts = shared(COUNT_WORDS as u64 * 4)?;
        counts.zero_fill();
        // Past each population's count the buffer is zero, from here on: the
        // split zeroes only what the slot's previous publish filled beyond
        // the new count, so the slot's counts must describe its buffers
        // from the first publish.
        let buffers = [shared(population)?, shared(population)?, shared(population)?, shared(population)?];
        for buffer in &buffers {
            buffer.zero_fill();
        }
        self.slots.push(OutputSlot {
            buffers,
            counts,
            written: None,
            read: 0,
        });
        Ok(Some(self.slots.len() - 1))
    }

    pub(crate) fn current(&self) -> Option<&OutputSlot> {
        self.published.map(|(index, _)| &self.slots[index])
    }
}

manifold_core::testkit_visible! {
/// The node's GPU side: pipelines, the pool and its fields, the outputs.
#[derive(Default)]
pub(crate) struct Step {
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    reference: reference::Reference,
    pipelines: Pipelines,
    surface_distance: manifold_water_liquid::primitives::whitewater_distance::SurfaceDistance,
    shape: Option<StepShape>,
    tick_mode: bool,
    epoch: Option<u32>,
    fields: Option<Fields>,
    particles: ParticleScratch,
    emission_scan: PrefixScan,
    slot_scan: PrefixScan,
    sort: ParticleSorter,
    /// Which of the two pool buffers holds the pool.
    current: usize,
    influence_epoch: Option<u32>,
    influence_current: usize,
    /// The pool stepped since an output last took it.
    owed: bool,
    pub(super) outputs: Outputs,
}
}

impl Step {
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn set_reference_capture_for_test(&mut self, capture: bool) { self.reference.capture = capture; }
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn set_reference_enabled_for_test(&mut self, enabled: bool) { self.reference.enabled = enabled; }
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn particle_snapshots_for_test(&self) -> Option<&[GpuBuffer; 3]> { self.reference.particle_snapshots.as_ref() }
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn turbulence_dispatches_for_test(&self) -> u32 { self.reference.turbulence_dispatches.get() }

    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn unpacked_faces_for_test(&self) -> Option<&[GpuBuffer; 3]> { self.fields().unpacked_faces.as_ref() }
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn turbulence_for_test(&self) -> &GpuBuffer { &self.fields().turbulence }


    /// One frame: on ticks publish what retired, start the pool over on a new shape
    /// or epoch, emit and step on ticks, and write an output when the pool
    /// changed.
    pub(crate) fn advance(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        fence: &dyn Fence,
        offline: bool,
        frame: &StepFrame,
        inputs: &StepInputs<'_>,
    ) -> Result<Report, String> {
        let device = gpu.device;
        self.pipelines.prepare(device);
        self.surface_distance.prepare(device);
        self.emission_scan.prepare(device);
        self.slot_scan.prepare(device);
        self.sort.prepare(device);
        let shape = frame.shape;
        require_inputs(&shape, inputs)?;
        if frame.dust_emission && inputs.obstacle_source.is_none() { return Err("dust emission requires nearest-object obstacle_source".to_owned()); }
        let reseed = self.shape != Some(shape) || self.tick_mode != inputs.distance.is_some() || self.epoch != Some(frame.epoch);
        self.reserve(device, shape, inputs.particles.size / PARTICLE, inputs.distance.is_some())?;
        if reseed {
            self.epoch = Some(frame.epoch);
            self.outputs.clear();
            self.owed = false;
            self.current = 0;
        }
        // Publication moves only with the clock. A held clock taking the
        // pending slot would change the picture once on the first paused
        // frame, and at slow speed the foam would step on frames the water
        // holds.
        if frame.ticks > 0 {
            self.outputs.collect(fence, offline)?;
        }
        self.outputs.mark_read(fence);
        let enc = &mut *gpu.native_enc;
        if reseed {
            self.seed(enc, &shape);
        }
        if frame.ticks > 0 {
            let slots = (inputs.particles.size / PARTICLE) as u32;
            let emitters = frame.count.map_or(slots, |count| count.min(slots));
            let scratch = self.particles.reserve(device, slots)?.clone();
            let offsets = self.emission_scan.buffer(device, emitters.max(1) as usize)?.clone();
            let surface = self.emit(enc, frame, inputs, &scratch, &offsets, emitters);
            for _ in 0..frame.ticks {
                self.tick(enc, device, frame, inputs, &surface)?;
            }
            self.owed = true;
        }
        if self.owed
            && let Some(index) = self.outputs.free(device, fence, &shape)?
        {
            self.publish(enc, &shape, index);
            self.outputs.slots[index].written = Some(fence.stamp());
            self.owed = false;
        }
        Ok(self.outputs.report)
    }

manifold_core::testkit_visible! {
    pub(crate) fn reserve(&mut self, device: &GpuDevice, shape: StepShape, particles: u64, tick_mode: bool) -> Result<(), String> {
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        self.reference.reserve(device, particles as u32, shape.capacity)?;
        self.surface_distance.reserve(device, shape.face_cells)?;
        if self.shape != Some(shape) || self.tick_mode != tick_mode {
            self.shape = None;
            self.fields = None;
            self.influence_epoch = None;
            self.outputs = Outputs::default();
            let held = shape.held_bytes(particles, tick_mode);
            manifold_node_engine::load::expand::admit_candidate_bytes(device.modifier_memory_snapshot(), held)
                .map_err(|error| format!("the pool and its scratch need {held} bytes: {error}"))?;
            let scan = self.slot_scan.buffer(device, shape.slot_scan_values())?.clone();
            self.fields = Some(Fields::new(device, &shape, tick_mode, scan)?);
            self.sort.reserve_ranges(device, shape.bins)?;
            self.shape = Some(shape);
            self.tick_mode = tick_mode;
        }
        Ok(())
    }
}

manifold_core::testkit_visible! {
    pub(crate) fn reserve_faces(&mut self, device: &GpuDevice, packed: bool) -> Result<(), String> {
        let f = self.fields.as_mut().expect("whitewater fields allocated");
        if packed {
            if f.unpacked_faces.is_none() {
                let bytes = self.shape.expect("shape allocated").unpacked_face_bytes() / 3;
                f.unpacked_faces = Some([allocate(device, bytes, false)?, allocate(device, bytes, false)?, allocate(device, bytes, false)?]);
            }
        } else {
            f.unpacked_faces = None;
        }
        Ok(())
    }
}

    fn face_axes<'a>(&'a self, faces: FaceSource<'a>) -> [&'a GpuBuffer; 3] {
        match faces {
            FaceSource::Axes(axes) => axes,
            FaceSource::Packed(_) => self.fields().unpacked_faces.as_ref().expect("packed faces reserved").each_ref(),
        }
    }

manifold_core::testkit_visible! {
    pub(crate) fn unpack_faces(&self, enc: &mut manifold_gpu::GpuEncoder, shape: &StepShape, faces: FaceSource<'_>) {
        if let FaceSource::Packed(packed) = faces {
            let [u, v, w] = self.face_axes(faces);
            let [nodes_x, nodes_y, nodes_z] = shape.face_cells.map(|n| (n + 4) as f32);
            let params = UnpackParams { nodes_x, nodes_y, nodes_z, count: (shape.unpacked_face_bytes() / 12) as u32 };
            let pass = Fused::UnpackFaces;
            dispatch(enc, &self.pipelines.fused[pass.index()], bytemuck::bytes_of(&params), &[packed, u, v, w],
                params.count, FUSED_ENTRIES[pass.index()].1, Barrier::After);
        }
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
        self.sort.prepare(gpu.device);
        self.reserve(gpu.device, shape, inputs.particles.size / PARTICLE, inputs.distance.is_some())?;
        self.reserve_faces(gpu.device, matches!(inputs.faces, FaceSource::Packed(_)))?;
        if self.outputs.slots.is_empty() {
            self.outputs.free(gpu.device, &Retired, &shape)?;
        }
        self.current = 0;
        let enc = &mut *gpu.native_enc;
        if enabled {
            self.unpack_faces(enc, &shape, inputs.faces);
            let f = self.fields();
            enc.copy_buffer_to_buffer(pool, &f.pools[0], shape.pool_bytes());
            enc.copy_buffer_to_buffer(state, &f.state, STATE_WORDS * 4);
            let slots = (inputs.particles.size / PARTICLE) as u32;
            let emitters = frame.count.map_or(slots, |count| count.min(slots));
            let scratch = self.particles.reserve(gpu.device, slots)?.clone();
            let offsets = self.emission_scan.buffer(gpu.device, emitters.max(1) as usize)?.clone();
            let surface = self.emit(enc, frame, inputs, &scratch, &offsets, emitters);
            self.tick(enc, gpu.device, frame, inputs, &surface)?;
        } else {
            self.influence_epoch = None;
            self.seed(enc, &shape);
        }
        self.publish(enc, &shape, 0);
        Ok(())
    }
}

manifold_core::testkit_visible! {
    pub(crate) fn tick_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "pool_out" => self.fields.as_ref().map(|f| &f.pools[self.current]),
            "state_out" => self.fields.as_ref().map(|f| &f.state),
            "counts_out" => self.outputs.slots.first().map(|s| &s.counts),
            _ => OUTPUTS.iter().position(|&name| name == port)
                .and_then(|i| self.outputs.slots.first().map(|s| &s.buffers[i])),
        }
    }
}

    /// Whitewater switched off: unpublish the pool so nothing downstream
    /// draws it, and start it over when it is switched on again. The
    /// buffers stay; a slot a frame still reads is not rewritten until
    /// that frame retires, as after any restart.
    pub(crate) fn stop(&mut self) {
        self.epoch = None;
        self.influence_epoch = None;
        self.outputs.clear();
        self.owed = false;
    }


    fn fields(&self) -> &Fields {
        self.fields.as_ref().expect("whitewater fields allocated")
    }

    /// A pass of `whitewater_step.wgsl` over `count` threads; `bound` fills
    /// bindings 1 to 10 in the shader's order.
    fn hand(&self, enc: &mut manifold_gpu::GpuEncoder, pass: Hand, params: HandParams, bound: [&GpuBuffer; 11]) {
        let (pipeline, label) = self.pipelines.hand(pass);
        dispatch(enc, pipeline, bytemuck::bytes_of(&params), &bound, params.count, label, Barrier::After);
    }

    /// Bindings for a hand pass: the pool pair, the slot scan, the typed
    /// spawns and the state, with `state` standing in for whatever the pass
    /// never touches.
    fn bound<'a>(&'a self, pool_in: &'a GpuBuffer, pool_out: &'a GpuBuffer, extra: HandBuffers<'a>) -> [&'a GpuBuffer; 11] {
        let f = self.fields();
        let filler = &f.state;
        let [foam, bubble, spray, dust] = extra.populations.unwrap_or([filler; 4]);
        [pool_in, pool_out, &f.scan, &f.typed, &f.state, extra.offsets.unwrap_or(filler), foam, bubble, spray, extra.counts.unwrap_or(filler), dust]
    }

    fn seed(&self, enc: &mut manifold_gpu::GpuEncoder, shape: &StepShape) {
        let f = self.fields();
        let params = HandParams { capacity: shape.capacity, spawn_slots: 0, emitters: 0, count: shape.capacity.max(STATE_WORDS as u32) };
        let bound = self.bound(&f.pools[1], &f.pools[0], HandBuffers::default());
        self.hand(enc, Hand::SeedPool, params, bound);
    }

    /// FLIP's emitter: the surface's distance and curvature from the level
    /// set, each liquid particle's energy and wavecrest potential, how many
    /// it emits, the spawns, their kinds, appended to the pool.
    fn emit(
        &mut self,
        enc: &mut manifold_gpu::GpuEncoder,
        frame: &StepFrame,
        inputs: &StepInputs<'_>,
        scratch: &[GpuBuffer; 5],
        offsets: &GpuBuffer,
        emitters: u32,
    ) -> GpuBuffer {
        let s = &frame.shape;
        let reset_influence = self.influence_epoch != Some(frame.epoch);
        self.influence_epoch = Some(frame.epoch);
        let influence_previous = self.influence_current;
        let influence_next = 1 - influence_previous;
        let f = self.fields();
        let p = &self.pipelines;
        let cells = s.cell_count() as u32;
        let [nx, ny, nz] = s.nodes.map(|n| n as f32);
        let [lx, ly, lz] = s.level_nodes.map(|n| n as f32);
        let nodes = [("nodes_x", nx), ("nodes_y", ny), ("nodes_z", nz)];
        let label = |pass: &str| -> &'static str {
            match pass {
                "crossings" => "node.whitewater_step.crossings",
                "spread" => "node.whitewater_step.spread",
                "distance" => "node.whitewater_step.distance",
                "liquid" => "node.whitewater_step.liquid_cells",
                "curvature" => "node.whitewater_step.curvature",
                _ => "node.whitewater_step.extend_curvature",
            }
        };
        // Resolve once for emit and lifecycle; the next encode overwrites
        // SurfaceDistance's current storage, so this view is never history.
        let (distance, surface) = if let Some(distance) = inputs.distance {
            let padding = face_offset(s.nodes, s.face_cells).expect("validated face placement")[0];
            if padding == 0 {
                let surface = self.surface_distance.encode(enc, distance, s.cell_size);
                enc.compute_memory_barrier_buffers();
                (distance, surface)
            } else {
                let padded_distance = f.distance.as_ref().expect("padded distance allocated");
                let padded_surface = f.surface.as_ref().expect("padded surface allocated");
                encode_pad_distance_lattice(enc, get(&p.pad_distance), distance, padded_distance,
                    s.face_cells, padding, 3.0 * s.cell_size);
                let surface = self.surface_distance.encode(enc, distance, s.cell_size);
                encode_pad_distance_lattice(enc, get(&p.pad_distance), surface, padded_surface,
                    s.face_cells, padding, 5.0 * s.cell_size);
                enc.compute_memory_barrier_buffers();
                (padded_distance, padded_surface)
            }
        } else {
            let distance = f.distance.as_ref().expect("legacy distance allocated");
            let surface = f.surface.as_ref().expect("legacy surface allocated");
            atom::<SurfaceCrossings>(
                enc,
                get(&p.crossings),
                &[nodes[0], nodes[1], nodes[2], ("level_nodes_x", lx), ("level_nodes_y", ly), ("level_nodes_z", lz)],
                &[inputs.level_set, inputs.solid, &f.crossings[0]],
                cells,
                label("crossings"),
            );
            let mut crossing = 0;
            for step in SPREAD_STEPS {
                atom::<NearestCrossing>(
                    enc,
                    get(&p.nearest),
                    &[nodes[0], nodes[1], nodes[2], ("step", step)],
                    &[&f.crossings[crossing], &f.crossings[1 - crossing]],
                    cells,
                    label("spread"),
                );
                crossing = 1 - crossing;
            }
            atom::<CrossingDistance>(
                enc,
                get(&p.distance),
                &[nodes[0], nodes[1], nodes[2], ("cell_size", s.cell_size)],
                &[&f.crossings[crossing], inputs.solid, distance],
                cells,
                label("distance"),
            );
            enc.copy_buffer_to_buffer(distance, surface, u64::from(cells) * 4);
            (distance, surface)
        };
        atom::<WhitewaterInfluence>(enc, get(&p.influence),
            &[("base_level", frame.influence_base), ("decay_rate", frame.influence_decay), ("dt", frame.dt),
              ("cell_size", s.cell_size), ("reset", f32::from(u8::from(reset_influence))),
              ("source_present", f32::from(u8::from(inputs.obstacle_source.is_some())))],
            &[&f.influence[influence_previous], inputs.solid, inputs.obstacle_source.unwrap_or(&f.empty_source), &f.influence[influence_next]],
            cell_total(s.nodes) as u32, "node.whitewater_step.influence");
        // Liquid cells and curvature both read the distance; neither reads
        // the other, so one barrier after the pair.
        atom_then::<LiquidCells>(enc, get(&p.liquid), &nodes, &[distance, inputs.solid, &f.cells], cells, label("liquid"), Barrier::None);
        atom::<LatticeCurvature>(
            enc,
            get(&p.curvature),
            &[nodes[0], nodes[1], nodes[2], ("cell_size", s.cell_size)],
            &[surface, &f.curvature[0]],
            cells,
            label("curvature"),
        );
        let mut curvature = 0;
        for _ in 0..3 {
            atom::<ExtendLattice>(enc, get(&p.extend), &nodes, &[&f.curvature[curvature], &f.curvature[1 - curvature]], cells, label("extend"));
            curvature = 1 - curvature;
        }
        let [sampled, energy, unscaled, dust_energy, wavecrest_bits] = scratch;
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        let reference = self.reference.enabled;
        self.turbulence(enc, frame, inputs, distance);
        let [u, v, w] = self.face_axes(inputs.faces);
        let fused = &p.fused;
        emitter_path!(reference, {
            self.reference.emit_reference(enc, frame, inputs, f, surface, curvature, influence_next, offsets, emitters);
        }, {
            let pass = Fused::Emit;
            dispatch(enc, &fused[pass.index()], bytemuck::bytes_of(&EmitParams::new(frame, emitters, false)),
                &[inputs.particles, u, v, w,
                  surface, &f.cells, &f.curvature[curvature], &f.turbulence, &f.influence[influence_next],
                  sampled, energy, offsets, unscaled, wavecrest_bits], emitters, FUSED_ENTRIES[pass.index()].1, Barrier::After);
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            self.reference.record_dispatch(FUSED_ENTRIES[pass.index()].1, emitters);
        });
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        let (sampled, energy, unscaled) = if reference {
            let [jittered, sampled, energy, _, _] = self.reference.scratch();
            (jittered, energy, sampled)
        } else { (sampled, energy, unscaled) };
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        self.reference.capture_emit(enc, sampled, unscaled, energy, offsets, emitters, frame.dust_emission);
        self.emission_scan.encode_labelled(enc, emitters.max(1) as usize, EMISSION_SCAN);
        self.spawn(enc, frame, inputs, surface, offsets, sampled, energy, emitters, false);
        let params = HandParams { capacity: s.capacity, spawn_slots: s.capacity, emitters, count: s.capacity };
        let pool = &f.pools[self.current];
        self.hand(enc, Hand::LiveFlags, params, self.bound(pool, pool, HandBuffers::default()));
        self.slot_scan.encode_labelled(enc, s.capacity as usize, APPEND_SCAN);
        self.hand(enc, Hand::Append, params, self.bound(pool, pool, HandBuffers::default()));
        let state = HandParams { count: 1, ..params };
        self.hand(enc, Hand::AppendState, state, self.bound(pool, pool, HandBuffers { offsets: Some(offsets), ..HandBuffers::default() }));
        if frame.dust_emission {
            emitter_path!(reference, {
                self.reference.dust_reference(enc, frame, inputs, f, influence_next, offsets, emitters);
            }, {
                let pass = Fused::Dust;
                dispatch(enc, &fused[pass.index()], bytemuck::bytes_of(&EmitParams::new(frame, emitters, true)),
                    &[unscaled, inputs.solid, inputs.obstacle_source.expect("validated dust source"),
                      &f.state, &f.state, &f.state, &f.state, &f.turbulence,
                      &f.influence[influence_next], &f.state, dust_energy, offsets, &f.state, wavecrest_bits],
                    emitters, FUSED_ENTRIES[pass.index()].1, Barrier::After);
                #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
                self.reference.record_dispatch(FUSED_ENTRIES[pass.index()].1, emitters);
            });
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            let dust_energy = if reference { self.reference.dust_energy() } else { dust_energy };
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            self.reference.capture_dust(enc, dust_energy, offsets, emitters);
            self.emission_scan.encode_labelled(enc, emitters.max(1) as usize, EMISSION_SCAN);
            self.spawn(enc, frame, inputs, surface, offsets, unscaled, dust_energy, emitters, true);
            self.hand(enc, Hand::LiveFlags, params, self.bound(pool, pool, HandBuffers::default()));
            self.slot_scan.encode_labelled(enc, s.capacity as usize, APPEND_SCAN);
            self.hand(enc, Hand::Append, params, self.bound(pool, pool, HandBuffers::default()));
            self.hand(enc, Hand::AppendState, state, self.bound(pool, pool, HandBuffers { offsets: Some(offsets), ..HandBuffers::default() }));
        }
        let surface = surface.clone();
        self.influence_current = influence_next;
        surface
    }

    fn turbulence(&self, enc: &mut manifold_gpu::GpuEncoder, frame: &StepFrame, inputs: &StepInputs<'_>, distance: &GpuBuffer) {
        let f = self.fields();
        emitter_path!(self.reference.enabled, {
            self.reference.turbulence_reference(enc, frame, inputs, distance, &f.turbulence);
        }, {
            let [u, v, w] = self.face_axes(inputs.faces);
            let s = &frame.shape;
            let [face_cells_x, face_cells_y, face_cells_z] = s.face_cells.map(|n| n as f32);
            let [nodes_x, nodes_y, nodes_z] = s.nodes.map(|n| n as f32);
            let params = TurbulenceParams { face_cells_x, face_cells_y, face_cells_z, nodes_x, nodes_y, nodes_z,
                cell_size: s.cell_size, count: s.cell_count() as u32 };
            let pass = Fused::Turbulence;
            dispatch(enc, &self.pipelines.fused[pass.index()], bytemuck::bytes_of(&params),
                &[&f.state, u, v, w, distance, &f.turbulence], params.count, FUSED_ENTRIES[pass.index()].1, Barrier::After);
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            self.reference.record_dispatch(FUSED_ENTRIES[pass.index()].1, params.count);
        });
    }

    /// Spawn and classify before append, preserving the atom defaults for dust typing.
    fn spawn(&self, enc: &mut manifold_gpu::GpuEncoder, frame: &StepFrame, inputs: &StepInputs<'_>,
        surface: &GpuBuffer, offsets: &GpuBuffer, sampled: &GpuBuffer, energy: &GpuBuffer, emitters: u32, dust: bool) {
        let f = self.fields();
        emitter_path!(self.reference.enabled, {
            self.reference.spawn_reference(enc, frame, inputs, f, surface, offsets, sampled, energy, emitters, dust);
        }, {
            let [u, v, w] = self.face_axes(inputs.faces);
            let pass = Fused::Spawn;
            let label = if dust { "node.whitewater_step.dust_spawn" } else { FUSED_ENTRIES[pass.index()].1 };
            dispatch(enc, &self.pipelines.fused[pass.index()], bytemuck::bytes_of(&SpawnParams::new(frame, emitters, dust)),
                &[sampled, u, v, w,
                  surface, &f.cells, offsets, energy, inputs.solid, &f.typed], frame.shape.capacity, label, Barrier::After);
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            self.reference.record_dispatch(label, frame.shape.capacity);
        });
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        self.reference.capture_particle(enc, &f.typed, usize::from(dust));
    }

    /// The old a → b → a → b chain ends in b in both paths. Sort and all
    /// current/preserve/compaction bookkeeping stay in tick.
    fn lifecycle(&self, enc: &mut manifold_gpu::GpuEncoder, frame: &StepFrame, inputs: &StepInputs<'_>,
        surface: &GpuBuffer, a: &GpuBuffer, b: &GpuBuffer) {
        let f = self.fields();
        emitter_path!(self.reference.enabled, {
            self.reference.lifecycle_reference(enc, frame, inputs, f, surface, a, b);
        }, {
            let motion = inputs.motion.as_ref();
            let empty = &f.state;
            let history = motion.map_or([empty; 3], |m| m.faces);
            let [u, v, w] = self.face_axes(inputs.faces);
            let pass = Fused::Lifecycle;
            dispatch(enc, &self.pipelines.fused[pass.index()], bytemuck::bytes_of(&LifecycleParams::new(frame, inputs)),
                &[a, u, v, w, surface, &f.cells, inputs.solid,
                  motion.map_or(empty, |m| m.schedule), history[0], history[1], history[2],
                  motion.and_then(|m| m.fields.forces).unwrap_or(empty), motion.and_then(|m| m.fields.impulses).unwrap_or(empty), b],
                frame.shape.capacity, FUSED_ENTRIES[pass.index()].1, Barrier::After);
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            self.reference.record_dispatch(FUSED_ENTRIES[pass.index()].1, frame.shape.capacity);
        });
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
        let f = self.fields.as_ref().expect("the step's fields are allocated");
        let p = &self.pipelines;
        let cap = s.capacity;
        let (a, b) = (&f.pools[self.current], &f.pools[1 - self.current]);
        let [nx, ny, nz] = s.nodes.map(|n| n as f32);
        let [cx, cy, cz] = s.center;
        let [sx, sy, sz] = s.size;
        let [bx, by, bz] = s.bins.map(|n| n as f32);
        let dt = frame.dt;
        let place = [
            ("center_x", cx),
            ("center_y", cy),
            ("center_z", cz),
            ("size_x", sx),
            ("size_y", sy),
            ("size_z", sz),
            ("nodes_x", nx),
            ("nodes_y", ny),
            ("nodes_z", nz),
        ];
        self.lifecycle(enc, frame, inputs, surface, a, b);
        let motion = inputs.motion.as_ref();
        let empty = &f.state;
        let bin_min: [f32; 3] = std::array::from_fn(|i| s.center[i] - 0.5 * s.size[i]);
        let job = SortJob {
            particles: b,
            read: whitewater_record_read(),
            capacity: cap,
            count: cap,
            bin_min,
            inv_cell: 1.0 / s.cell_size,
            bins: s.bins,
            sorted: None,
            order: Some(&f.order),
            gate: None,
        };
        self.sort.encode(device, enc, &job, &SORT_LABELS)?;
        let ranges = self.sort.ranges().expect("ranges reserved with the shape");
        let bins = [("bins_x", bx), ("bins_y", by), ("bins_z", bz)];
        // Preserve foam off is the identity (FLIP skips the pass,
        // diffuseparticlesimulation.cpp:2124), so the aged pool `b` goes
        // straight to the keep and the compaction lands back in `a`; on, the
        // preserved pool is `a` and the compaction lands in `b`. Either way
        // `current` ends on the compacted pool.
        let (stepped, out) = if frame.preserve_foam {
            let mut preserve = [("", 0.0); 12];
            preserve[0] = ("enabled", 1.0);
            preserve[1] = ("dt", dt);
            preserve[2..8].copy_from_slice(&place[..6]);
            preserve[8] = ("cell_size", s.cell_size);
            preserve[9] = bins[0];
            preserve[10] = bins[1];
            preserve[11] = bins[2];
            atom::<PreserveFoam>(
                enc,
                get(&p.preserve),
                &preserve,
                &[b, b, ranges, &f.order, a],
                cap,
                "node.whitewater_step.preserve_foam",
            );
            (a, b)
        } else {
            (b, a)
        };
        let mut keep = [("", 0.0); 15];
        keep[..9].copy_from_slice(&place[..9]);
        keep[9..12].copy_from_slice(&bins);
        keep[12..].copy_from_slice(&[
            ("region_count", motion.map_or(0.0, |m| m.region_count as f32)),
            ("region_offset", motion.map_or(0.0, |m| (m.tick_index - m.fields.first_tick as f32).max(0.0) * m.region_count as f32)),
            ("tick_seconds", frame.dt),
        ]);
        atom::<KeepWhitewater>(
            enc,
            get(&p.keep),
            &keep,
            &[stepped, stepped, ranges, &f.order, inputs.solid, motion.and_then(|m| m.regions).unwrap_or(empty), motion.and_then(|m| m.shapes).unwrap_or(empty), motion.and_then(|m| m.atlas).unwrap_or(empty), &f.scan],
            cap,
            "node.whitewater_step.keep",
        );
        self.slot_scan.encode_labelled(enc, cap as usize, KEEP_SCAN);
        let params = HandParams { capacity: cap, spawn_slots: 0, emitters: 0, count: cap };
        self.hand(enc, Hand::Compact, params, self.bound(stepped, out, HandBuffers::default()));
        self.hand(enc, Hand::CompactState, HandParams { count: 1, ..params }, self.bound(stepped, out, HandBuffers::default()));
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
        let extra = || HandBuffers { populations: Some([foam, bubble, spray, dust]), counts: Some(&slot.counts), ..HandBuffers::default() };
        let params = HandParams { capacity: shape.capacity, spawn_slots: 0, emitters: 0, count: 4 * shape.capacity };
        self.hand(enc, Hand::SplitFlags, params, self.bound(pool, pool, extra()));
        self.slot_scan.encode_labelled(enc, shape.slot_scan_values(), SPLIT_SCAN);
        self.hand(enc, Hand::Split, params, self.bound(pool, pool, extra()));
        self.hand(enc, Hand::PublishCounts, HandParams { count: 1, ..params }, self.bound(pool, pool, extra()));
    }
}

/// Buffers a hand pass binds beyond the step's own.
#[derive(Default)]
struct HandBuffers<'a> {
    offsets: Option<&'a GpuBuffer>,
    populations: Option<[&'a GpuBuffer; 4]>,
    counts: Option<&'a GpuBuffer>,
}

impl WhitewaterStep {
    fn frame(ctx: &EffectNodeContext<'_, '_>) -> Result<StepFrame, String> {
        let whole = |v: f32| v.round().max(0.0) as u32;
        let triple = |names: [&str; 3]| names.map(|name| whole(ctx.scalar_or_param(name, 0.0)));
        let capacity = ctx.scalar_or_param("capacity", DEFAULT_CAPACITY as f32).round();
        if !(1.0..=MAX_CAPACITY as f32).contains(&capacity) {
            return Err(format!("capacity {capacity} is outside 1 to {MAX_CAPACITY}"));
        }
        let amount = ctx.param_f32("amount", 1.0);
        if !amount.is_finite() || amount < 0.0 {
            return Err(format!("Whitewater Amount {amount} must be 0 or more"));
        }
        let shape = StepShape::new(
            triple(["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"]),
            if ctx.inputs.slot("distance").is_some() {
                triple(["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"])
            } else {
                triple(["level_set_nodes_x", "level_set_nodes_y", "level_set_nodes_z"])
            },
            triple(["face_cells_x", "face_cells_y", "face_cells_z"]),
            ctx.scalar_or_param("face_valid_layers", 0.0),
            ctx.inputs.transform("grid_bounds"),
            capacity as u32,
        )?;
        let count = ctx.inputs.scalar("count").map(|_| whole(ctx.scalar_or_param("count", 0.0)));
        let (min_t, max_t) = (ctx.param_f32("min_turbulence", 100.0), ctx.param_f32("max_turbulence", 200.0));
        if !min_t.is_finite() || !max_t.is_finite() || min_t < 0.0 || max_t <= min_t {
            return Err("Whitewater: Max Turbulence must exceed nonnegative Min Turbulence".to_owned());
        }
        Ok(StepFrame {
            // A graph saved without the domain's interval wire runs on the project's Sim Rate.
            dt: ctx.scalar_or_param("dt", ctx.sim_step.interval.0 as f32),
            shape,
            count,
            ticks: if ctx.inputs.slot("distance").is_some() { 1 } else { whole(ctx.scalar_or_param("ticks", 0.0)) },
            epoch: whole(ctx.scalar_or_param("epoch", 0.0)),
            seed: if ctx.inputs.slot("distance").is_some() { ctx.scalar_or_param("tick_index", 0.0) * TICK as f32 } else { ctx.scalar_or_param("seed", 0.0) },
            gravity: [ctx.scalar_or_param("gravity_x", 0.0), ctx.scalar_or_param("gravity", -9.81), ctx.scalar_or_param("gravity_z", 0.0)],
            wavecrest_emission: ctx.param_f32("wavecrest_emission", WAVECREST_RATE) * amount,
            dust_emission: matches!(ctx.params.get("dust_emission"), Some(ParamValue::Bool(true))),
            boundary_dust: matches!(ctx.params.get("boundary_dust"), Some(ParamValue::Bool(true))),
            dust_rate: ctx.param_f32("dust_rate", 175.0) * amount,
            influence_base: ctx.param_f32("influence_base", 1.0),
            influence_decay: ctx.param_f32("influence_decay", 2.0),
            spray_speed: ctx.param_f32("spray_speed", 1.0),
            generation_rate: ctx.param_f32("generation_rate", 1.0),
            turbulence_emission: ctx.param_f32("turbulence_emission", 175.0) * amount,
            min_turbulence: ctx.param_f32("min_turbulence", 100.0),
            max_turbulence: ctx.param_f32("max_turbulence", 200.0),
            inside_emission: !matches!(ctx.params.get("inside_emission"), Some(ParamValue::Bool(false))),
            min_energy: ctx.param_f32("min_energy", MIN_ENERGY),
            max_energy: ctx.param_f32("max_energy", MAX_ENERGY),
            preserve_foam: matches!(ctx.params.get("preserve_foam"), Some(ParamValue::Bool(true))),
        })
    }
}

impl Primitive for WhitewaterStep {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.step.pipelines.prepare(device);
        self.step.surface_distance.prepare(device);
        self.step.emission_scan.prepare(device);
        self.step.slot_scan.prepare(device);
        self.step.sort.prepare(device);
    }

    fn provides_array_output(&self, port: &str) -> bool {
        OUTPUTS.contains(&port) || matches!(port, "pool_out" | "state_out" | "counts_out")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        if let Some(axis) = ["proof_unpack_u", "proof_unpack_v", "proof_unpack_w"].iter().position(|&p| p == port) {
            assert!(self.step.reference.capture);
            return self.step.fields.as_ref()?.unpacked_faces.as_ref().map(|axes| &axes[axis]);
        }
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        if port == "proof_turbulence" {
            assert!(self.step.reference.capture && self.step.reference.turbulence_dispatches.get() > 0);
            return self.step.fields.as_ref().map(|f| &f.turbulence);
        }
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        if let Some(buffer) = self.step.reference.output(port) { return Some(buffer); }
        if self.tick_mode {
            return self.step.tick_output(port);
        }
        let population = OUTPUTS.iter().position(|&name| name == port)?;
        self.step.outputs.current().map(|slot| &slot.buffers[population])
    }

    /// Each population output holds the whole pool.
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        if port == "counts_out" { return Some(COUNT_WORDS as u32); }
        if port == "state_out" { return Some(STATE_WORDS as u32); }
        (OUTPUTS.contains(&port) || port == "pool_out").then(|| match params.get("capacity") {
            Some(ParamValue::Float(v)) => (v.round().max(1.0) as u32).min(MAX_CAPACITY),
            _ => DEFAULT_CAPACITY,
        })
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.tick_mode = ctx.inputs.slot("distance").is_some();
        let packed = match packed_face_source(self.tick_mode, ctx.inputs.slot("faces").is_some(),
            ["face_u", "face_v", "face_w"].map(|port| ctx.inputs.slot(port).is_some())) {
            Ok(packed) => packed,
            Err(refusal) => { ctx.error(refusal); return; }
        };
        if !self.tick_mode && ctx.param_f32("enabled", 1.0) < 0.5 {
            self.step.stop();
            for name in OUTPUT_COUNTS.iter().chain(["dust_count", "emitted", "thinned", "pool_full"].iter()) {
                ctx.outputs.set_scalar(name, ParamValue::Float(0.0));
            }
            return;
        }
        let frame = match Self::frame(ctx) {
            Ok(frame) => frame,
            Err(refusal) => {
                ctx.error(format!("Whitewater Step: {refusal}"));
                return;
            }
        };
        let (Some(particles), Some(solid), Some(level_set)) = (
            ctx.inputs.array("particles"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("distance").or_else(|| ctx.inputs.array("level_set")),
        ) else {
            ctx.error("Whitewater Step: particles, solid and distance or level_set must all be wired");
            return;
        };
        let faces = if packed {
            let Some(faces) = ctx.inputs.array("faces") else { ctx.error("Whitewater Step: faces buffer is unavailable"); return; };
            FaceSource::Packed(faces)
        } else {
            let [Some(u), Some(v), Some(w)] = ["face_u", "face_v", "face_w"].map(|port| ctx.inputs.array(port)) else {
                ctx.error("Whitewater Step: face_u, face_v and face_w buffers are unavailable"); return;
            };
            FaceSource::Axes([u, v, w])
        };
        let motion = if let Some(schedule) = ctx.inputs.array("substep_schedule") {
            let [Some(u), Some(v), Some(w)] = ["substep_u", "substep_v", "substep_w"].map(|n| ctx.inputs.array(n)) else {
                ctx.error("Whitewater Step: an accepted schedule requires all three substep face histories"); return;
            };
            let fields = match FieldBinding::read(ctx, ctx.inputs.array("forces"), ctx.inputs.array("impulses"), "Whitewater Step") {
                Ok(fields) => fields, Err(error) => { ctx.error(error); return; }
            };
            Some(MotionInputs { schedule, faces: [u,v,w], count: ctx.scalar_or_param("substep_count",0.0) as u32,
                fields, tick_index: ctx.scalar_or_param("tick_index",0.0), regions: ctx.inputs.array("regions"),
                shapes: ctx.inputs.array("shapes"), atlas: ctx.inputs.array("atlas"), region_count: ctx.scalar_or_param("region_count",0.0) as u32 })
        } else { None };
        let inputs = StepInputs { motion, particles, solid, obstacle_source: ctx.inputs.array("obstacle_source"), faces, level_set, distance: ctx.inputs.array("distance") };
        if self.tick_mode {
            let (Some(pool), Some(state)) = (ctx.inputs.array("pool"), ctx.inputs.array("pool_state")) else {
                ctx.error("Whitewater Step: per-tick distance requires pool and pool_state from the liquid boundary");
                return;
            };
            let enabled = ctx.param_f32("enabled", 1.0) >= 0.5;
            if let Err(error) = self.step.advance_tick(ctx.gpu_encoder(), &frame, &inputs, pool, state, enabled) {
                ctx.error(format!("Whitewater Step: {error}"));
            }
            return;
        }
        let offline = ctx.sim_step.offline();
        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();
        let fence: &dyn Fence = match &clock {
            Some(clock) => clock,
            None => &Retired,
        };
        let report = match self.step.advance(gpu, fence, offline, &frame, &inputs) {
            Ok(report) => report,
            Err(refusal) => {
                ctx.error(format!("Whitewater Step: {refusal}"));
                return;
            }
        };
        for (name, count) in OUTPUT_COUNTS.into_iter().zip(report.counts) {
            ctx.outputs.set_scalar(name, ParamValue::Float(count as f32));
        }
        ctx.outputs.set_scalar("dust_count", ParamValue::Float(report.dust as f32));
        ctx.outputs.set_scalar("emitted", ParamValue::Float(report.emitted as f32));
        ctx.outputs.set_scalar("thinned", ParamValue::Float(report.thinned as f32));
        ctx.outputs.set_scalar("pool_full", ParamValue::Float(report.pool_full as f32));
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod copy_tests;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(any(test, feature = "testkit"))]
pub fn empty_slot() -> WhitewaterParticle {
    WhitewaterParticle { kind: manifold_water_liquid::whitewater::WHITEWATER_EMPTY, ..Default::default() }
}
