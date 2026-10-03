//! `node.whitewater_step` — FLIP whitewater emission and lifecycle.
//! GPU FLIP runs this stage once per liquid tick using solver particle phi.
//! The liquid boundary captures the pool, IDs, counters and rendering arrays
//! after each tick; the stage owns reusable scratch. The legacy level-set
//! interface retains its frame publication ring for existing saved graphs.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_fluids::WhitewaterSpawn;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use super::age_whitewater::AgeWhitewater;
use super::advect_whitewater::AdvectWhitewater;
use super::crossing_distance::CrossingDistance;
use super::emission_count::WAVECREST_RATE;
use super::turbulence_emission_count::TurbulenceEmissionCount;
use super::turbulence_field::TurbulenceField;
use super::whitewater_influence::WhitewaterInfluence;
use super::whitewater_obstacle_source::WhitewaterSource;
use super::dust_potential::DustPotential;
use super::whitewater_emitter_velocity::WhitewaterEmitterVelocity;
use super::inside_turbulence_potential::InsideTurbulencePotential;
use super::energy_potential::{EnergyPotential, MAX_ENERGY, MIN_ENERGY};
use super::extend_lattice::ExtendLattice;
use super::jitter_particles::JitterParticles;
use super::keep_whitewater::KeepWhitewater;
use super::lattice_curvature::LatticeCurvature;
use super::liquid_cells::LiquidCells;
use super::nearest_crossing::NearestCrossing;
use super::pad_distance_lattice::{encode_pad_distance_lattice, prepare_pad_distance_lattice};
use super::prefix_scan::{PrefixScan, ScanLabels, storage_words};
use super::preserve_foam::PreserveFoam;
use super::retype_whitewater::RetypeWhitewater;
use super::sample_faces_at_particles::SampleFacesAtParticles;
use super::sort_particles_into_cells::{ParticleSorter, SortJob, SortLabels, range_storage_bytes, whitewater_record_read};
use super::spawn_whitewater::SpawnWhitewater;
use super::standalone_pipeline::standalone_pipeline;
use super::surface_crossings::SurfaceCrossings;
use super::wavecrest_potential::WavecrestPotential;
use super::whitewater_type::WhitewaterType;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid::TICK;
use crate::node_graph::fluid_particles::{FluidParticle, MAX_BINS, bin_counts, bin_total};
use crate::node_graph::liquid::grid::face_len;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::physics::offline_simulation;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::transform::Transform;
use crate::node_graph::whitewater::{
    KnownValue, SPREAD_STEPS, SURFACE_CROSSING_BYTES, WhitewaterParticle, cell_total, face_offset, grid_box, grid_cells, refinement,
    require_extended_faces,
};
use crate::node_graph::whitewater_handoff::{Fence, Retired};

/// FLIP's own default whitewater budget.
pub const DEFAULT_CAPACITY: u32 = 100_000;
pub const MAX_CAPACITY: u32 = 250_000;

pub(crate) const WHITEWATER_STEP_SHADER: &str = include_str!("shaders/whitewater_step.wgsl");

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

crate::primitive! {
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
        face_u: Array(f32) required, face_v: Array(f32) required, face_w: Array(f32) required,
        face_cells_x: ScalarF32 optional, face_cells_y: ScalarF32 optional, face_cells_z: ScalarF32 optional,
        face_valid_layers: ScalarF32 optional,
        level_set: Array(f32) optional,
        distance: Array(f32) optional,
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
    composition_notes: "For per-tick GPU FLIP, wire the solver distance, particles and projected face components; wire pool and pool_state from liquid_state, and close pool_out, state_out, counts_out and the three population arrays through its capture results. tick_index is the boundary clock; each invocation emits and advances one tick. The boundary publishes capacity-sized populations with zero-radius unused records, so particles_to_copies needs no CPU live_count. Wire capacity to both the stage and boundary. The legacy level_set interface keeps its frame ticks, seed, scalar counts and retired output ring for saved graphs. Amount scales emission, while zero leaves the existing pool advancing; disabling clears the pool on the next tick.",
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
    /// slots, every scratch, scan, sort and output slot.
    pub fn held_bytes(&self, particles: u64) -> u64 {
        let cells = self.cell_count();
        let grid = cells * (2 * SURFACE_CROSSING_BYTES + 4 + 4 + 2 * KNOWN_VALUE + 4);
        let per_particle = particles * (2 * PARTICLE + 4 + 4 + 4) + storage_words(particles as usize) as u64 * 4;
        let capacity = u64::from(self.capacity);
        let pool = 2 * self.spawn_bytes() + 2 * self.pool_bytes() + capacity * 4;
        let sort = 2 * capacity * 4 + storage_words(bin_total(self.bins) as usize) as u64 * 4 + self.range_bytes();
        let scan = storage_words(self.slot_scan_values()) as u64 * 4 + STATE_WORDS * 4;
        let outputs = OUTPUT_SLOTS as u64 * (4 * self.population_bytes() + COUNT_WORDS as u64 * 4);
        grid + per_particle + pool + sort + scan + outputs + 6 * self.solid_bytes()
    }
}

/// One frame's scalar inputs and knobs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StepFrame {
    pub shape: StepShape,
    /// Live liquid particles; `None` takes every slot.
    pub count: Option<u32>,
    pub ticks: u32,
    /// Duration of each accepted interval; export supplies exactly 1/60 s.
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

pub(crate) struct StepInputs<'a> {
    pub particles: &'a GpuBuffer,
    pub solid: &'a GpuBuffer,
    pub obstacle_source: Option<&'a GpuBuffer>,
    pub faces: [&'a GpuBuffer; 3],
    pub level_set: &'a GpuBuffer,
    pub distance: Option<&'a GpuBuffer>,
}

/// Each input holds at least what the shape reads from it.
fn require_inputs(shape: &StepShape, inputs: &StepInputs<'_>) -> Result<(), String> {
    if inputs.obstacle_source.is_some_and(|source| source.size < shape.solid_bytes() * 4) {
        return Err("obstacle source is shorter than the solid lattice".to_owned());
    }
    let wanted = [
        ("solid", inputs.solid, shape.solid_bytes()),
        ("liquid distance", inputs.distance.unwrap_or(inputs.level_set), if inputs.distance.is_some() { cell_total(shape.face_cells) * 4 } else { shape.level_bytes() }),
        ("face_u", inputs.faces[0], shape.face_bytes(0)),
        ("face_v", inputs.faces[1], shape.face_bytes(1)),
        ("face_w", inputs.faces[2], shape.face_bytes(2)),
    ];
    for (name, buffer, bytes) in wanted {
        if buffer.size < bytes {
            return Err(format!("{name} holds {} bytes, fewer than the {bytes} its grid reads", buffer.size));
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
    words: [u32; 32],
    len: usize,
}

fn pack<P: Primitive>(values: &[(&str, f32)], count: u32) -> Uniform {
    let params = P::PARAMS;
    debug_assert!(
        values.iter().all(|(name, _)| params.iter().any(|p| p.name == *name)),
        "{} takes no param among {values:?}",
        P::TYPE_ID
    );
    let mut words = [0u32; 32];
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
    let bindings: [GpuBinding<'_>; 12] = std::array::from_fn(|i| match i {
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
    turbulence: Option<GpuComputePipeline>,
    inside: Option<GpuComputePipeline>,
    emitter_velocity: Option<GpuComputePipeline>,
    influence: Option<GpuComputePipeline>,
    dust: Option<GpuComputePipeline>,
    extend: Option<GpuComputePipeline>,
    jitter: Option<GpuComputePipeline>,
    sample: Option<GpuComputePipeline>,
    energy: Option<GpuComputePipeline>,
    wavecrest: Option<GpuComputePipeline>,
    emission: Option<GpuComputePipeline>,
    spawn: Option<GpuComputePipeline>,
    kind: Option<GpuComputePipeline>,
    advect: Option<GpuComputePipeline>,
    retype: Option<GpuComputePipeline>,
    age: Option<GpuComputePipeline>,
    preserve: Option<GpuComputePipeline>,
    keep: Option<GpuComputePipeline>,
    /// `whitewater_step.wgsl`, in [`Hand`] order.
    hand: Vec<GpuComputePipeline>,
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
        standalone_pipeline::<DustPotential>(&mut self.dust, device);
        standalone_pipeline::<WhitewaterEmitterVelocity>(&mut self.emitter_velocity, device);
        standalone_pipeline::<TurbulenceField>(&mut self.turbulence, device);
        standalone_pipeline::<InsideTurbulencePotential>(&mut self.inside, device);
        standalone_pipeline::<LatticeCurvature>(&mut self.curvature, device);
        standalone_pipeline::<ExtendLattice>(&mut self.extend, device);
        standalone_pipeline::<JitterParticles>(&mut self.jitter, device);
        standalone_pipeline::<SampleFacesAtParticles>(&mut self.sample, device);
        standalone_pipeline::<EnergyPotential>(&mut self.energy, device);
        standalone_pipeline::<WavecrestPotential>(&mut self.wavecrest, device);
        standalone_pipeline::<TurbulenceEmissionCount>(&mut self.emission, device);
        standalone_pipeline::<SpawnWhitewater>(&mut self.spawn, device);
        standalone_pipeline::<WhitewaterType>(&mut self.kind, device);
        standalone_pipeline::<AdvectWhitewater>(&mut self.advect, device);
        standalone_pipeline::<RetypeWhitewater>(&mut self.retype, device);
        standalone_pipeline::<AgeWhitewater>(&mut self.age, device);
        standalone_pipeline::<PreserveFoam>(&mut self.preserve, device);
        standalone_pipeline::<KeepWhitewater>(&mut self.keep, device);
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
    crate::node_graph::scene_modifier_expand::admit_candidate_bytes(device.modifier_memory_snapshot(), bytes)
        .map_err(|error| error.to_string())?;
    let bytes = bytes.max(16);
    if shared { device.try_create_buffer_shared(bytes) } else { device.try_create_buffer(bytes) }
}

/// The grid fields and the pool, sized from one [`StepShape`].
struct Fields {
    crossings: [GpuBuffer; 2],
    distance: GpuBuffer,
    cells: GpuBuffer,
    curvature: [GpuBuffer; 2],
    turbulence: GpuBuffer,
    influence: [GpuBuffer; 2],
    empty_source: GpuBuffer,
    spawns: GpuBuffer,
    typed: GpuBuffer,
    pools: [GpuBuffer; 2],
    order: GpuBuffer,
    state: GpuBuffer,
    /// The slot scan's storage, level 0 at offset 0.
    scan: GpuBuffer,
}

impl Fields {
    fn new(device: &GpuDevice, shape: &StepShape, scan: GpuBuffer) -> Result<Self, String> {
        let cells = shape.cell_count();
        let alloc = |bytes| allocate(device, bytes, false);
        Ok(Self {
            crossings: [alloc(cells * SURFACE_CROSSING_BYTES)?, alloc(cells * SURFACE_CROSSING_BYTES)?],
            distance: alloc(cells * 4)?,
            cells: alloc(cells * 4)?,
            curvature: [alloc(cells * KNOWN_VALUE)?, alloc(cells * KNOWN_VALUE)?],
            turbulence: alloc(cells * 4)?,
            influence: [alloc(shape.solid_bytes())?, alloc(shape.solid_bytes())?],
            empty_source: alloc(shape.solid_bytes() * 4)?,
            spawns: alloc(shape.spawn_bytes())?,
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
    buffers: Option<[GpuBuffer; 5]>,
}

impl ParticleScratch {
    fn reserve(&mut self, device: &GpuDevice, slots: u32) -> Result<&[GpuBuffer; 5], String> {
        if self.buffers.is_none() || self.slots < slots {
            let n = u64::from(slots);
            let alloc = |bytes| allocate(device, bytes, false);
            self.buffers = Some([alloc(n * PARTICLE)?, alloc(n * PARTICLE)?, alloc(n * 4)?, alloc(n * 4)?, alloc(n * 4)?]);
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

/// The node's GPU side: pipelines, the pool and its fields, the outputs.
#[derive(Default)]
pub(crate) struct Step {
    pipelines: Pipelines,
    shape: Option<StepShape>,
    epoch: Option<u32>,
    fields: Option<Fields>,
    particles: ParticleScratch,
    emission_scan: PrefixScan,
    slot_scan: PrefixScan,
    sort: ParticleSorter,
    /// Which of the two pool buffers holds the pool.
    current: usize,
    influence_epoch: Option<u32>,
    /// The pool stepped since an output last took it.
    owed: bool,
    pub(super) outputs: Outputs,
}

impl Step {
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
        self.emission_scan.prepare(device);
        self.slot_scan.prepare(device);
        self.sort.prepare(device);
        let shape = frame.shape;
        require_inputs(&shape, inputs)?;
        if frame.dust_emission && inputs.obstacle_source.is_none() { return Err("dust emission requires nearest-object obstacle_source".to_owned()); }
        let reseed = self.shape != Some(shape) || self.epoch != Some(frame.epoch);
        self.reserve(device, shape, inputs.particles.size / PARTICLE)?;
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
            self.emit(enc, frame, inputs, &scratch, &offsets, emitters);
            for _ in 0..frame.ticks {
                self.tick(enc, device, frame, inputs)?;
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

    fn reserve(&mut self, device: &GpuDevice, shape: StepShape, particles: u64) -> Result<(), String> {
        if self.shape != Some(shape) {
            self.shape = None;
            self.fields = None;
            self.influence_epoch = None;
            self.outputs = Outputs::default();
            let held = shape.held_bytes(particles);
            crate::node_graph::scene_modifier_expand::admit_candidate_bytes(device.modifier_memory_snapshot(), held)
                .map_err(|error| format!("the pool and its scratch need {held} bytes: {error}"))?;
            let scan = self.slot_scan.buffer(device, shape.slot_scan_values())?.clone();
            self.fields = Some(Fields::new(device, &shape, scan)?);
            self.sort.reserve_ranges(device, shape.bins)?;
            self.shape = Some(shape);
        }
        Ok(())
    }

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
        self.emission_scan.prepare(gpu.device);
        self.slot_scan.prepare(gpu.device);
        self.sort.prepare(gpu.device);
        self.reserve(gpu.device, shape, inputs.particles.size / PARTICLE)?;
        if self.outputs.slots.is_empty() {
            self.outputs.free(gpu.device, &Retired, &shape)?;
        }
        self.current = 0;
        let enc = &mut *gpu.native_enc;
        if enabled {
            let f = self.fields();
            enc.copy_buffer_to_buffer(pool, &f.pools[0], shape.pool_bytes());
            enc.copy_buffer_to_buffer(state, &f.state, STATE_WORDS * 4);
            let slots = (inputs.particles.size / PARTICLE) as u32;
            let emitters = frame.count.map_or(slots, |count| count.min(slots));
            let scratch = self.particles.reserve(gpu.device, slots)?.clone();
            let offsets = self.emission_scan.buffer(gpu.device, emitters.max(1) as usize)?.clone();
            self.emit(enc, frame, inputs, &scratch, &offsets, emitters);
            self.tick(enc, gpu.device, frame, inputs)?;
        } else {
            self.influence_epoch = None;
            self.seed(enc, &shape);
        }
        self.publish(enc, &shape, 0);
        Ok(())
    }

    pub(crate) fn tick_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "pool_out" => self.fields.as_ref().map(|f| &f.pools[self.current]),
            "state_out" => self.fields.as_ref().map(|f| &f.state),
            "counts_out" => self.outputs.slots.first().map(|s| &s.counts),
            _ => OUTPUTS.iter().position(|&name| name == port)
                .and_then(|i| self.outputs.slots.first().map(|s| &s.buffers[i])),
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
    ) {
        let s = &frame.shape;
        let reset_influence = self.influence_epoch != Some(frame.epoch);
        self.influence_epoch = Some(frame.epoch);
        let f = self.fields();
        let p = &self.pipelines;
        let cells = s.cell_count() as u32;
        let [nx, ny, nz] = s.nodes.map(|n| n as f32);
        let [lx, ly, lz] = s.level_nodes.map(|n| n as f32);
        let [cx, cy, cz] = s.center;
        let [sx, sy, sz] = s.size;
        let [fx, fy, fz] = s.face_cells.map(|n| n as f32);
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
        if let Some(distance) = inputs.distance {
            let padding = face_offset(s.nodes, s.face_cells).expect("validated face placement")[0];
            encode_pad_distance_lattice(enc, get(&p.pad_distance), distance, &f.distance,
                s.face_cells, padding, 3.0 * s.cell_size);
            enc.compute_memory_barrier_buffers();
        } else {
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
                &[&f.crossings[crossing], inputs.solid, &f.distance],
                cells,
                label("distance"),
            );
        }
        atom::<WhitewaterInfluence>(enc, get(&p.influence),
            &[("base_level", frame.influence_base), ("decay_rate", frame.influence_decay), ("dt", frame.dt),
              ("cell_size", s.cell_size), ("reset", f32::from(u8::from(reset_influence))),
              ("source_present", f32::from(u8::from(inputs.obstacle_source.is_some())))],
            &[&f.influence[0], inputs.solid, inputs.obstacle_source.unwrap_or(&f.empty_source), &f.influence[1]],
            cell_total(s.nodes) as u32, "node.whitewater_step.influence");
        enc.copy_buffer_to_buffer(&f.influence[1], &f.influence[0], s.solid_bytes());
        // Liquid cells and curvature both read the distance; neither reads
        // the other, so one barrier after the pair.
        atom_then::<LiquidCells>(enc, get(&p.liquid), &nodes, &[&f.distance, inputs.solid, &f.cells], cells, label("liquid"), Barrier::None);
        atom::<LatticeCurvature>(
            enc,
            get(&p.curvature),
            &[nodes[0], nodes[1], nodes[2], ("cell_size", s.cell_size)],
            &[&f.distance, &f.curvature[0]],
            cells,
            label("curvature"),
        );
        let mut curvature = 0;
        for _ in 0..3 {
            atom::<ExtendLattice>(enc, get(&p.extend), &nodes, &[&f.curvature[curvature], &f.curvature[1 - curvature]], cells, label("extend"));
            curvature = 1 - curvature;
        }
        let [jittered, sampled, energy, wavecrest, inside] = scratch;
        let box3 = [("center_x", cx), ("center_y", cy), ("center_z", cz), ("size_x", sx), ("size_y", sy), ("size_z", sz)];
        let faces = [("face_cells_x", fx), ("face_cells_y", fy), ("face_cells_z", fz)];
        atom::<TurbulenceField>(
            enc, get(&p.turbulence),
            &[faces[0], faces[1], faces[2], nodes[0], nodes[1], nodes[2], ("cell_size", s.cell_size)],
            &[&f.distance, inputs.faces[0], inputs.faces[1], inputs.faces[2], &f.turbulence],
            cells, "node.whitewater_step.turbulence");
        let epoch = frame.epoch as f32;
        // The per-particle passes run over the emitters, not every slot of
        // the particle array: the emission count masks everything from the
        // live count up, and the spawn reads the scratch only below the
        // emission scan's length.
        atom::<JitterParticles>(
            enc,
            get(&p.jitter),
            &[("cell_size", s.cell_size), ("seed", frame.seed), ("epoch", epoch)],
            &[inputs.particles, jittered],
            emitters,
            "node.whitewater_step.jitter",
        );
        let mut sample = [("", 0.0); 12];
        sample[..3].copy_from_slice(&faces);
        sample[3..9].copy_from_slice(&box3);
        sample[9..].copy_from_slice(&nodes);
        atom::<SampleFacesAtParticles>(
            enc,
            get(&p.sample),
            &sample,
            &[jittered, inputs.faces[0], inputs.faces[1], inputs.faces[2], sampled],
            emitters,
            "node.whitewater_step.sample_velocity",
        );
        let mut grid = [("", 0.0); 9];
        grid[..6].copy_from_slice(&box3);
        grid[6..].copy_from_slice(&nodes);
        let mut velocity_params = [("", 0.0); 12];
        velocity_params[..9].copy_from_slice(&grid);
        velocity_params[9] = ("spray_speed", frame.spray_speed);
        velocity_params[10] = ("seed", frame.seed);
        velocity_params[11] = ("epoch", epoch);
        atom::<WhitewaterEmitterVelocity>(enc, get(&p.emitter_velocity), &velocity_params,
            &[sampled, &f.distance, &f.cells, jittered], emitters, "node.whitewater_step.emitter_velocity");
        let unscaled = sampled;
        let sampled = jittered;
        atom_then::<EnergyPotential>(
            enc, get(&p.energy),
            &[("min_energy", frame.min_energy), ("max_energy", frame.max_energy)],
            &[sampled, energy], emitters, "node.whitewater_step.energy", Barrier::None);
        atom::<WavecrestPotential>(
            enc,
            get(&p.wavecrest),
            &grid,
            &[sampled, &f.distance, &f.curvature[curvature], &f.cells, wavecrest],
            emitters,
            "node.whitewater_step.wavecrest",
        );
        let mut turbulence_params = [("", 0.0); 12];
        turbulence_params[..9].copy_from_slice(&grid);
        turbulence_params[9] = ("min_turbulence", frame.min_turbulence);
        turbulence_params[10] = ("max_turbulence", frame.max_turbulence);
        turbulence_params[11] = ("inside_enabled", f32::from(u8::from(frame.inside_emission)));
        atom::<InsideTurbulencePotential>(enc, get(&p.inside), &turbulence_params,
            &[sampled, &f.distance, &f.turbulence, &f.cells, inside], emitters, "node.whitewater_step.inside");
        let mut count_params = [("", 0.0); 18];
        count_params[..8].copy_from_slice(&[("rate", frame.wavecrest_emission), ("turbulence_rate", frame.turbulence_emission), ("generation_rate", frame.generation_rate), ("seed", frame.seed), ("epoch", epoch), ("points_per_cell", 8.0), ("ticks", frame.ticks as f32), ("live_count", emitters as f32)]);
        count_params[8..17].copy_from_slice(&grid);
        count_params[17] = ("dt", frame.dt);
        atom::<TurbulenceEmissionCount>(
            enc,
            get(&p.emission),
            &count_params,
            &[sampled, energy, wavecrest, inside, &f.influence[1], offsets],
            emitters,
            "node.whitewater_step.emission",
        );
        self.emission_scan.encode_labelled(enc, emitters.max(1) as usize, EMISSION_SCAN);
        let mut spawn = [("", 0.0); 19];
        spawn[0] = ("capacity", s.capacity as f32);
        spawn[1] = ("emitters", emitters as f32);
        spawn[2..5].copy_from_slice(&faces);
        spawn[5..11].copy_from_slice(&box3);
        spawn[11..14].copy_from_slice(&nodes);
        spawn[14] = ("seed", frame.seed);
        spawn[15] = ("epoch", epoch);
        spawn[16] = ("dt", frame.dt);
        atom::<SpawnWhitewater>(
            enc,
            get(&p.spawn),
            &spawn[..17],
            &[offsets, sampled, energy, inputs.faces[0], inputs.faces[1], inputs.faces[2], inputs.solid, &f.spawns],
            s.capacity,
            "node.whitewater_step.spawn",
        );
        atom::<WhitewaterType>(enc, get(&p.kind), &velocity_params, &[&f.spawns, &f.distance, &f.cells, &f.typed], s.capacity, "node.whitewater_step.kind");
        let params = HandParams { capacity: s.capacity, spawn_slots: s.capacity, emitters, count: s.capacity };
        let pool = &f.pools[self.current];
        self.hand(enc, Hand::LiveFlags, params, self.bound(pool, pool, HandBuffers::default()));
        self.slot_scan.encode_labelled(enc, s.capacity as usize, APPEND_SCAN);
        self.hand(enc, Hand::Append, params, self.bound(pool, pool, HandBuffers::default()));
        let state = HandParams { count: 1, ..params };
        self.hand(enc, Hand::AppendState, state, self.bound(pool, pool, HandBuffers { offsets: Some(offsets), ..HandBuffers::default() }));
        if frame.dust_emission {
            let mut dust_params = [("", 0.0); 13];
            dust_params[..9].copy_from_slice(&grid);
            dust_params[9..].copy_from_slice(&[("min_turbulence", frame.min_turbulence), ("max_turbulence", frame.max_turbulence),
                ("dust_enabled", 1.0), ("boundary_dust", f32::from(u8::from(frame.boundary_dust)))]);
            atom::<DustPotential>(enc, get(&p.dust), &dust_params,
                &[unscaled, inputs.solid, &f.turbulence, inputs.obstacle_source.expect("validated dust source"), inside],
                emitters, "node.whitewater_step.dust_potential");
            atom::<EnergyPotential>(enc, get(&p.energy),
                &[("min_energy", frame.min_energy), ("max_energy", frame.max_energy)],
                &[unscaled, energy], emitters, "node.whitewater_step.dust_energy");
            count_params[0] = ("rate", 0.0);
            count_params[1] = ("turbulence_rate", frame.dust_rate);
            count_params[3] = ("seed", frame.seed + 104729.0);
            atom::<TurbulenceEmissionCount>(enc, get(&p.emission), &count_params,
                &[unscaled, energy, wavecrest, inside, &f.influence[1], offsets], emitters, "node.whitewater_step.dust_count");
            self.emission_scan.encode_labelled(enc, emitters.max(1) as usize, EMISSION_SCAN);
            spawn[14] = ("seed", frame.seed + 104729.0);
            atom::<SpawnWhitewater>(enc, get(&p.spawn), &spawn[..17],
                &[offsets, unscaled, energy, inputs.faces[0], inputs.faces[1], inputs.faces[2], inputs.solid, &f.spawns],
                s.capacity, "node.whitewater_step.dust_spawn");
            let mut dust_type = [("", 0.0); 10];
            dust_type[..9].copy_from_slice(&grid);
            dust_type[9] = ("dust", 1.0);
            atom::<WhitewaterType>(enc, get(&p.kind), &dust_type,
                &[&f.spawns, &f.distance, &f.cells, &f.typed], s.capacity, "node.whitewater_step.dust_type");
            self.hand(enc, Hand::LiveFlags, params, self.bound(pool, pool, HandBuffers::default()));
            self.slot_scan.encode_labelled(enc, s.capacity as usize, APPEND_SCAN);
            self.hand(enc, Hand::Append, params, self.bound(pool, pool, HandBuffers::default()));
            self.hand(enc, Hand::AppendState, state, self.bound(pool, pool, HandBuffers { offsets: Some(offsets), ..HandBuffers::default() }));
        }

    }

    /// One tick of FLIP's whitewater: advect, retype, age, sort, preserve
    /// foam, mark the slots to keep, compact.
    fn tick(
        &mut self,
        enc: &mut manifold_gpu::GpuEncoder,
        device: &GpuDevice,
        frame: &StepFrame,
        inputs: &StepInputs<'_>,
    ) -> Result<(), String> {
        let s = &frame.shape;
        let f = self.fields.as_ref().expect("the step's fields are allocated");
        let p = &self.pipelines;
        let cap = s.capacity;
        let (a, b) = (&f.pools[self.current], &f.pools[1 - self.current]);
        let [nx, ny, nz] = s.nodes.map(|n| n as f32);
        let [cx, cy, cz] = s.center;
        let [sx, sy, sz] = s.size;
        let [fx, fy, fz] = s.face_cells.map(|n| n as f32);
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
            ("face_cells_x", fx),
            ("face_cells_y", fy),
            ("face_cells_z", fz),
        ];
        let [gx, gy, gz] = frame.gravity;
        let mut advect = [("", 0.0); 16];
        advect[..12].copy_from_slice(&place);
        advect[12..].copy_from_slice(&[("gravity_x", gx), ("gravity_y", gy), ("gravity_z", gz), ("dt", dt)]);
        let faces = inputs.faces;
        atom::<AdvectWhitewater>(enc, get(&p.advect), &advect, &[a, faces[0], faces[1], faces[2], inputs.solid, b], cap, "node.whitewater_step.advect");
        atom::<RetypeWhitewater>(
            enc,
            get(&p.retype),
            &place,
            &[b, &f.distance, &f.cells, faces[0], faces[1], faces[2], a],
            cap,
            "node.whitewater_step.retype",
        );
        atom::<AgeWhitewater>(enc, get(&p.age), &[("dt", dt)], &[a, b], cap, "node.whitewater_step.age");
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
        let mut keep = [("", 0.0); 12];
        keep[..9].copy_from_slice(&place[..9]);
        keep[9..].copy_from_slice(&bins);
        atom::<KeepWhitewater>(
            enc,
            get(&p.keep),
            &keep,
            &[stepped, stepped, ranges, &f.order, inputs.solid, &f.scan],
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
            dt: ctx.scalar_or_param("dt", TICK as f32),
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
        self.step.emission_scan.prepare(device);
        self.step.slot_scan.prepare(device);
        self.step.sort.prepare(device);
    }

    fn provides_array_output(&self, port: &str) -> bool {
        OUTPUTS.contains(&port) || matches!(port, "pool_out" | "state_out" | "counts_out")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
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
        let (Some(particles), Some(solid), Some(face_u), Some(face_v), Some(face_w), Some(level_set)) = (
            ctx.inputs.array("particles"),
            ctx.inputs.array("solid"),
            ctx.inputs.array("face_u"),
            ctx.inputs.array("face_v"),
            ctx.inputs.array("face_w"),
            ctx.inputs.array("distance").or_else(|| ctx.inputs.array("level_set")),
        ) else {
            ctx.error("Whitewater Step: particles, solid, face_u/v/w and level_set must all be wired");
            return;
        };
        let inputs = StepInputs { particles, solid, obstacle_source: ctx.inputs.array("obstacle_source"), faces: [face_u, face_v, face_w], level_set, distance: ctx.inputs.array("distance") };
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
        let offline = offline_simulation();
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
