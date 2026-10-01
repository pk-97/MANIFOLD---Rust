//! `node.whitewater_step` — FLIP's whitewater on the GPU, emit and lifecycle
//! in one node (`docs/GPU_WHITEWATER_DESIGN.md` section 3.9). Each frame
//! with ticks it reads the liquid's level set, faces and particles, emits
//! spawns into its own pool, then steps the pool once per tick: advect,
//! retype, age, sort, preserve foam, remove, compact. Out come foam, bubbles
//! and spray as particle frames.
//!
//! One node because the pool is cross-frame state with its own id counter
//! and counts, and every pass reads what the last wrote. Inside, every
//! barrier-free pass is a codegen atom's standalone kernel; the cell sort is
//! the sort node's own passes; the seed, append, compaction and split are
//! `shaders/whitewater_step.wgsl`, atomic-free.
//!
//! The counts the outputs carry are read back from the GPU, so the outputs
//! are published once the frame that wrote them retired: one frame behind
//! offline, which waits for it, and one to three live, which never waits.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_fluids::WhitewaterSpawn;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

use super::age_whitewater::AgeWhitewater;
use super::advect_whitewater::AdvectWhitewater;
use super::crossing_distance::CrossingDistance;
use super::emission_count::{EmissionCount, WAVECREST_RATE};
use super::energy_potential::{EnergyPotential, MAX_ENERGY, MIN_ENERGY};
use super::extend_lattice::ExtendLattice;
use super::jitter_particles::JitterParticles;
use super::keep_whitewater::KeepWhitewater;
use super::lattice_curvature::LatticeCurvature;
use super::liquid_cells::LiquidCells;
use super::nearest_crossing::NearestCrossing;
use super::prefix_scan::{PrefixScan, storage_words};
use super::preserve_foam::PreserveFoam;
use super::retype_whitewater::RetypeWhitewater;
use super::sample_faces_at_particles::SampleFacesAtParticles;
use super::sort_particles_into_cells::{CellSort, CellSortPass, range_storage_bytes, whitewater_record_read};
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

pub(crate) const OUTPUTS: [&str; 3] = ["foam_particles", "bubble_particles", "spray_particles"];

/// Published outputs in flight or on show; four cover live's three frames
/// behind and the one being written.
pub(crate) const OUTPUT_SLOTS: usize = 4;

/// Words the GPU writes per output slot: foam, bubble and spray counts,
/// emitted, thinned, pool full, live, next id.
const COUNT_WORDS: usize = 8;
const STATE_WORDS: u64 = 8;

const PARTICLE: u64 = std::mem::size_of::<FluidParticle>() as u64;
const POOL_SLOT: u64 = std::mem::size_of::<WhitewaterParticle>() as u64;
const SPAWN: u64 = std::mem::size_of::<WhitewaterSpawn>() as u64;
const KNOWN_VALUE: u64 = std::mem::size_of::<KnownValue>() as u64;

crate::primitive! {
    name: WhitewaterStep,
    type_id: "node.whitewater_step",
    purpose: "Foam, bubbles and spray for a GPU liquid, all on the GPU as FLIP's whitewater: each frame with ticks, emit from where the surface crests and the water moves fast, then step the pool once per tick (spray falls and bounces, bubbles rise and drag, foam rides the surface, each ages, dies, and leaves when it strays or crowds its cell). Out come foam, bubbles and spray as particle frames, each particle's radius its fade, with their counts. The outputs trail the water by one frame offline and up to three live. A new epoch, grid or capacity clears the pool; ticks 0 holds it. Spawns past the pool's room are counted as pool full; emissions past Capacity in one frame are thinned evenly and counted.",
    inputs: {
        particles: Array(FluidParticle) required,
        count: ScalarF32 optional,
        solid: Array(f32) required,
        grid_bounds: Transform optional,
        grid_nodes_x: ScalarF32 optional, grid_nodes_y: ScalarF32 optional, grid_nodes_z: ScalarF32 optional,
        face_u: Array(f32) required, face_v: Array(f32) required, face_w: Array(f32) required,
        face_cells_x: ScalarF32 optional, face_cells_y: ScalarF32 optional, face_cells_z: ScalarF32 optional,
        face_valid_layers: ScalarF32 optional,
        level_set: Array(f32) required,
        level_set_nodes_x: ScalarF32 optional, level_set_nodes_y: ScalarF32 optional, level_set_nodes_z: ScalarF32 optional,
        ticks: ScalarF32 optional,
        epoch: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        seed: ScalarF32 optional,
    },
    outputs: {
        foam_particles: Array(FluidParticle),
        bubble_particles: Array(FluidParticle),
        spray_particles: Array(FluidParticle),
        foam_count: ScalarF32,
        bubble_count: ScalarF32,
        spray_count: ScalarF32,
        emitted: ScalarF32,
        thinned: ScalarF32,
        pool_full: ScalarF32,
    },
    params: [
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
    composition_notes: "Wire everything from the liquid: particles and count from its particle frame (count_b), solid, grid_bounds and grid_nodes_x/y/z from the particle frame's solid lattice, face_u/v/w, face_cells_x/y/z and face_valid_layers from its face grid (at least one valid layer), level_set and level_set_nodes_x/y/z from its level set (a whole refinement of the lattice), ticks, epoch and gravity_x/gravity/gravity_z from the domain, seed from anything that changes per run (simulation time). The face grid must sit centred on the lattice's cells by a whole number of cells. Draw each particles output with node.particles_to_copies, its count wired to live_count. emitted, thinned and pool_full count since the epoch began.",
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
        3 * self.capacity as usize
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
        let grid = cells * (2 * SURFACE_CROSSING_BYTES + 4 + 4 + 2 * KNOWN_VALUE);
        let per_particle = particles * (2 * PARTICLE + 4 + 4) + storage_words(particles as usize) as u64 * 4;
        let capacity = u64::from(self.capacity);
        let pool = 2 * self.spawn_bytes() + 2 * self.pool_bytes() + capacity * 4;
        let sort = 2 * capacity * 4 + storage_words(bin_total(self.bins) as usize) as u64 * 4 + self.range_bytes();
        let scan = storage_words(self.slot_scan_values()) as u64 * 4 + STATE_WORDS * 4;
        let outputs = OUTPUT_SLOTS as u64 * (3 * self.population_bytes() + COUNT_WORDS as u64 * 4);
        grid + per_particle + pool + sort + scan + outputs
    }
}

/// One frame's scalar inputs and knobs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StepFrame {
    pub shape: StepShape,
    /// Live liquid particles; `None` takes every slot.
    pub count: Option<u32>,
    pub ticks: u32,
    pub epoch: u32,
    pub seed: f32,
    pub gravity: [f32; 3],
    pub wavecrest_emission: f32,
    pub min_energy: f32,
    pub max_energy: f32,
    pub preserve_foam: bool,
}

pub(crate) struct StepInputs<'a> {
    pub particles: &'a GpuBuffer,
    pub solid: &'a GpuBuffer,
    pub faces: [&'a GpuBuffer; 3],
    pub level_set: &'a GpuBuffer,
}

/// Each input holds at least what the shape reads from it.
fn require_inputs(shape: &StepShape, inputs: &StepInputs<'_>) -> Result<(), String> {
    let wanted = [
        ("solid", inputs.solid, shape.solid_bytes()),
        ("level_set", inputs.level_set, shape.level_bytes()),
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
            emitted: words[3],
            thinned: words[4],
            pool_full: words[5],
            live: words[6],
            next_id: words[7],
        }
    }
}

/// A codegen atom's uniform: its params in order, each overridden by name or
/// at its default, then the dispatch count, padded to 16 bytes.
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
    words[params.len()] = count;
    Uniform { words, len: (params.len() + 1).next_multiple_of(4) }
}

/// Dispatch `pipeline` over `count` threads, the uniform at 0 and `buffers`
/// from 1 in order, then a barrier.
fn dispatch(enc: &mut manifold_gpu::GpuEncoder, pipeline: &GpuComputePipeline, uniform: &[u8], buffers: &[&GpuBuffer], count: u32, label: &str) {
    if count == 0 {
        return;
    }
    let bindings: [GpuBinding<'_>; 12] = std::array::from_fn(|i| match i {
        0 => GpuBinding::Bytes { binding: 0, data: uniform },
        i if i <= buffers.len() => GpuBinding::Buffer { binding: i as u32, buffer: buffers[i - 1], offset: 0 },
        _ => GpuBinding::Bytes { binding: 0, data: uniform },
    });
    enc.dispatch_compute(pipeline, &bindings[..=buffers.len()], [count.div_ceil(256), 1, 1], label);
    enc.compute_memory_barrier_buffers();
}

fn atom<P: Primitive>(
    enc: &mut manifold_gpu::GpuEncoder,
    pipeline: &GpuComputePipeline,
    values: &[(&str, f32)],
    buffers: &[&GpuBuffer],
    count: u32,
    label: &str,
) {
    let uniform = pack::<P>(values, count);
    dispatch(enc, pipeline, bytemuck::cast_slice(&uniform.words[..uniform.len]), buffers, count, label);
}

#[derive(Default)]
struct Pipelines {
    crossings: Option<GpuComputePipeline>,
    nearest: Option<GpuComputePipeline>,
    distance: Option<GpuComputePipeline>,
    liquid: Option<GpuComputePipeline>,
    curvature: Option<GpuComputePipeline>,
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

const SORT_LABELS: [&str; 6] = [
    "node.whitewater_step.sort.clear",
    "node.whitewater_step.sort.count",
    "node.whitewater_step.sort.ranges",
    "node.whitewater_step.sort.tail",
    "node.whitewater_step.sort.scatter",
    "node.whitewater_step.sort.stabilise",
];

impl Pipelines {
    fn prepare(&mut self, device: &GpuDevice) {
        standalone_pipeline::<SurfaceCrossings>(&mut self.crossings, device);
        standalone_pipeline::<NearestCrossing>(&mut self.nearest, device);
        standalone_pipeline::<CrossingDistance>(&mut self.distance, device);
        standalone_pipeline::<LiquidCells>(&mut self.liquid, device);
        standalone_pipeline::<LatticeCurvature>(&mut self.curvature, device);
        standalone_pipeline::<ExtendLattice>(&mut self.extend, device);
        standalone_pipeline::<JitterParticles>(&mut self.jitter, device);
        standalone_pipeline::<SampleFacesAtParticles>(&mut self.sample, device);
        standalone_pipeline::<EnergyPotential>(&mut self.energy, device);
        standalone_pipeline::<WavecrestPotential>(&mut self.wavecrest, device);
        standalone_pipeline::<EmissionCount>(&mut self.emission, device);
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
    spawns: GpuBuffer,
    typed: GpuBuffer,
    pools: [GpuBuffer; 2],
    order: GpuBuffer,
    ranges: GpuBuffer,
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
            spawns: alloc(shape.spawn_bytes())?,
            typed: alloc(shape.spawn_bytes())?,
            pools: [alloc(shape.pool_bytes())?, alloc(shape.pool_bytes())?],
            order: alloc(u64::from(shape.capacity) * 4)?,
            ranges: alloc(shape.range_bytes())?,
            state: alloc(STATE_WORDS * 4)?,
            scan,
        })
    }
}

/// Per liquid particle scratch, grown to the particle array.
#[derive(Default)]
struct ParticleScratch {
    slots: u32,
    buffers: Option<[GpuBuffer; 4]>,
}

impl ParticleScratch {
    fn reserve(&mut self, device: &GpuDevice, slots: u32) -> Result<&[GpuBuffer; 4], String> {
        if self.buffers.is_none() || self.slots < slots {
            let n = u64::from(slots);
            let alloc = |bytes| allocate(device, bytes, false);
            self.buffers = Some([alloc(n * PARTICLE)?, alloc(n * PARTICLE)?, alloc(n * 4)?, alloc(n * 4)?]);
            self.slots = slots;
        }
        Ok(self.buffers.as_ref().expect("particle scratch allocated"))
    }
}

pub(crate) struct OutputSlot {
    pub buffers: [GpuBuffer; 3],
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
        self.slots.push(OutputSlot {
            buffers: [shared(population)?, shared(population)?, shared(population)?],
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
    sort: CellSort,
    /// Which of the two pool buffers holds the pool.
    current: usize,
    /// The pool stepped since an output last took it.
    owed: bool,
    outputs: Outputs,
}

impl Step {
    /// One frame: publish what retired, start the pool over on a new shape
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
        let reseed = self.shape != Some(shape) || self.epoch != Some(frame.epoch);
        if self.shape != Some(shape) {
            self.shape = None;
            self.fields = None;
            self.outputs = Outputs::default();
            let held = shape.held_bytes(inputs.particles.size / PARTICLE);
            crate::node_graph::scene_modifier_expand::admit_candidate_bytes(device.modifier_memory_snapshot(), held)
                .map_err(|error| format!("the pool and its scratch need {held} bytes: {error}"))?;
            let scan = self.slot_scan.buffer(device, shape.slot_scan_values())?.clone();
            self.fields = Some(Fields::new(device, &shape, scan)?);
            self.sort.reserve(device, bin_total(shape.bins) as u32, shape.capacity)?;
            self.shape = Some(shape);
        }
        if reseed {
            self.epoch = Some(frame.epoch);
            self.outputs.clear();
            self.owed = false;
            self.current = 0;
        }
        self.outputs.collect(fence, offline)?;
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
            self.emit(enc, frame, inputs, &scratch, &offsets, slots, emitters);
            for _ in 0..frame.ticks {
                self.tick(enc, frame, inputs);
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


    fn fields(&self) -> &Fields {
        self.fields.as_ref().expect("whitewater fields allocated")
    }

    /// A pass of `whitewater_step.wgsl` over `count` threads; `bound` fills
    /// bindings 1 to 10 in the shader's order.
    fn hand(&self, enc: &mut manifold_gpu::GpuEncoder, pass: Hand, params: HandParams, bound: [&GpuBuffer; 10]) {
        let (pipeline, label) = self.pipelines.hand(pass);
        dispatch(enc, pipeline, bytemuck::bytes_of(&params), &bound, params.count, label);
    }

    /// Bindings for a hand pass: the pool pair, the slot scan, the typed
    /// spawns and the state, with `state` standing in for whatever the pass
    /// never touches.
    fn bound<'a>(&'a self, pool_in: &'a GpuBuffer, pool_out: &'a GpuBuffer, extra: HandBuffers<'a>) -> [&'a GpuBuffer; 10] {
        let f = self.fields();
        let filler = &f.state;
        let [foam, bubble, spray] = extra.populations.unwrap_or([filler; 3]);
        [pool_in, pool_out, &f.scan, &f.typed, &f.state, extra.offsets.unwrap_or(filler), foam, bubble, spray, extra.counts.unwrap_or(filler)]
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
        &self,
        enc: &mut manifold_gpu::GpuEncoder,
        frame: &StepFrame,
        inputs: &StepInputs<'_>,
        scratch: &[GpuBuffer; 4],
        offsets: &GpuBuffer,
        slots: u32,
        emitters: u32,
    ) {
        let s = &frame.shape;
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
        atom::<LiquidCells>(enc, get(&p.liquid), &nodes, &[&f.distance, inputs.solid, &f.cells], cells, label("liquid"));
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
        let [jittered, sampled, energy, wavecrest] = scratch;
        let box3 = [("center_x", cx), ("center_y", cy), ("center_z", cz), ("size_x", sx), ("size_y", sy), ("size_z", sz)];
        let faces = [("face_cells_x", fx), ("face_cells_y", fy), ("face_cells_z", fz)];
        let epoch = frame.epoch as f32;
        atom::<JitterParticles>(
            enc,
            get(&p.jitter),
            &[("cell_size", s.cell_size), ("seed", frame.seed), ("epoch", epoch)],
            &[inputs.particles, jittered],
            slots,
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
            slots,
            "node.whitewater_step.sample_velocity",
        );
        atom::<EnergyPotential>(
            enc,
            get(&p.energy),
            &[("min_energy", frame.min_energy), ("max_energy", frame.max_energy)],
            &[sampled, energy],
            slots,
            "node.whitewater_step.energy",
        );
        let mut grid = [("", 0.0); 9];
        grid[..6].copy_from_slice(&box3);
        grid[6..].copy_from_slice(&nodes);
        atom::<WavecrestPotential>(
            enc,
            get(&p.wavecrest),
            &grid,
            &[sampled, &f.distance, &f.curvature[curvature], &f.cells, wavecrest],
            slots,
            "node.whitewater_step.wavecrest",
        );
        atom::<EmissionCount>(
            enc,
            get(&p.emission),
            &[("rate", frame.wavecrest_emission), ("points_per_cell", 8.0), ("ticks", frame.ticks as f32), ("live_count", emitters as f32)],
            &[sampled, energy, wavecrest, offsets],
            emitters,
            "node.whitewater_step.emission",
        );
        self.emission_scan.encode(enc, emitters.max(1) as usize);
        let mut spawn = [("", 0.0); 19];
        spawn[0] = ("capacity", s.capacity as f32);
        spawn[1] = ("emitters", emitters as f32);
        spawn[2..5].copy_from_slice(&faces);
        spawn[5..11].copy_from_slice(&box3);
        spawn[11..14].copy_from_slice(&nodes);
        spawn[14] = ("seed", frame.seed);
        spawn[15] = ("epoch", epoch);
        atom::<SpawnWhitewater>(
            enc,
            get(&p.spawn),
            &spawn[..16],
            &[offsets, sampled, energy, inputs.faces[0], inputs.faces[1], inputs.faces[2], inputs.solid, &f.spawns],
            s.capacity,
            "node.whitewater_step.spawn",
        );
        atom::<WhitewaterType>(enc, get(&p.kind), &grid, &[&f.spawns, &f.distance, &f.cells, &f.typed], s.capacity, "node.whitewater_step.kind");
        let params = HandParams { capacity: s.capacity, spawn_slots: s.capacity, emitters, count: s.capacity };
        let pool = &f.pools[self.current];
        self.hand(enc, Hand::LiveFlags, params, self.bound(pool, pool, HandBuffers::default()));
        self.slot_scan.encode(enc, s.capacity as usize);
        self.hand(enc, Hand::Append, params, self.bound(pool, pool, HandBuffers::default()));
        let state = HandParams { count: 1, ..params };
        self.hand(enc, Hand::AppendState, state, self.bound(pool, pool, HandBuffers { offsets: Some(offsets), ..HandBuffers::default() }));
    }

    /// One tick of FLIP's whitewater: advect, retype, age, sort, preserve
    /// foam, mark the slots to keep, compact.
    fn tick(&mut self, enc: &mut manifold_gpu::GpuEncoder, frame: &StepFrame, inputs: &StepInputs<'_>) {
        let s = &frame.shape;
        let f = self.fields();
        let p = &self.pipelines;
        let cap = s.capacity;
        let (a, b) = (&f.pools[self.current], &f.pools[1 - self.current]);
        let [nx, ny, nz] = s.nodes.map(|n| n as f32);
        let [cx, cy, cz] = s.center;
        let [sx, sy, sz] = s.size;
        let [fx, fy, fz] = s.face_cells.map(|n| n as f32);
        let [bx, by, bz] = s.bins.map(|n| n as f32);
        let dt = TICK as f32;
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
        let pass = CellSortPass {
            particles: b,
            read: whitewater_record_read(),
            count: cap,
            bin_min,
            inv_cell: 1.0 / s.cell_size,
            bins: s.bins,
            ranges: &f.ranges,
            sorted: None,
            order: Some(&f.order),
            sorted_capacity: cap,
        };
        self.sort.encode(enc, &pass, &SORT_LABELS);
        let bins = [("bins_x", bx), ("bins_y", by), ("bins_z", bz)];
        let mut preserve = [("", 0.0); 12];
        preserve[0] = ("enabled", f32::from(u8::from(frame.preserve_foam)));
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
            &[b, b, &f.ranges, &f.order, a],
            cap,
            "node.whitewater_step.preserve_foam",
        );
        let mut keep = [("", 0.0); 12];
        keep[..9].copy_from_slice(&place[..9]);
        keep[9..].copy_from_slice(&bins);
        atom::<KeepWhitewater>(enc, get(&p.keep), &keep, &[a, a, &f.ranges, &f.order, inputs.solid, &f.scan], cap, "node.whitewater_step.keep");
        self.slot_scan.encode(enc, cap as usize);
        let params = HandParams { capacity: cap, spawn_slots: 0, emitters: 0, count: cap };
        self.hand(enc, Hand::Compact, params, self.bound(a, b, HandBuffers::default()));
        self.hand(enc, Hand::CompactState, HandParams { count: 1, ..params }, self.bound(a, b, HandBuffers::default()));
        self.current = 1 - self.current;
    }

    /// Split the pool into foam, bubbles and spray in output slot `index`,
    /// and its counts beside them.
    fn publish(&self, enc: &mut manifold_gpu::GpuEncoder, shape: &StepShape, index: usize) {
        let f = self.fields();
        let slot = &self.outputs.slots[index];
        let pool = &f.pools[self.current];
        let [foam, bubble, spray] = &slot.buffers;
        let extra = || HandBuffers { populations: Some([foam, bubble, spray]), counts: Some(&slot.counts), ..HandBuffers::default() };
        let params = HandParams { capacity: shape.capacity, spawn_slots: 0, emitters: 0, count: 3 * shape.capacity };
        self.hand(enc, Hand::SplitFlags, params, self.bound(pool, pool, extra()));
        self.slot_scan.encode(enc, shape.slot_scan_values());
        self.hand(enc, Hand::Split, params, self.bound(pool, pool, extra()));
        self.hand(enc, Hand::PublishCounts, HandParams { count: 1, ..params }, self.bound(pool, pool, extra()));
    }
}

/// Buffers a hand pass binds beyond the step's own.
#[derive(Default)]
struct HandBuffers<'a> {
    offsets: Option<&'a GpuBuffer>,
    populations: Option<[&'a GpuBuffer; 3]>,
    counts: Option<&'a GpuBuffer>,
}

impl WhitewaterStep {
    fn frame(ctx: &EffectNodeContext<'_, '_>) -> Result<StepFrame, String> {
        let whole = |v: f32| v.round().max(0.0) as u32;
        let triple = |names: [&str; 3]| names.map(|name| whole(ctx.scalar_or_param(name, 0.0)));
        let capacity = ctx.param_f32("capacity", DEFAULT_CAPACITY as f32).round();
        if !(1.0..=MAX_CAPACITY as f32).contains(&capacity) {
            return Err(format!("capacity {capacity} is outside 1 to {MAX_CAPACITY}"));
        }
        let shape = StepShape::new(
            triple(["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"]),
            triple(["level_set_nodes_x", "level_set_nodes_y", "level_set_nodes_z"]),
            triple(["face_cells_x", "face_cells_y", "face_cells_z"]),
            ctx.scalar_or_param("face_valid_layers", 0.0),
            ctx.inputs.transform("grid_bounds"),
            capacity as u32,
        )?;
        let count = ctx.inputs.scalar("count").map(|_| whole(ctx.scalar_or_param("count", 0.0)));
        Ok(StepFrame {
            shape,
            count,
            ticks: whole(ctx.scalar_or_param("ticks", 0.0)),
            epoch: whole(ctx.scalar_or_param("epoch", 0.0)),
            seed: ctx.scalar_or_param("seed", 0.0),
            gravity: [ctx.scalar_or_param("gravity_x", 0.0), ctx.scalar_or_param("gravity", -9.81), ctx.scalar_or_param("gravity_z", 0.0)],
            wavecrest_emission: ctx.param_f32("wavecrest_emission", WAVECREST_RATE),
            min_energy: ctx.param_f32("min_energy", MIN_ENERGY),
            max_energy: ctx.param_f32("max_energy", MAX_ENERGY),
            preserve_foam: matches!(ctx.params.get("preserve_foam"), Some(ParamValue::Bool(true))),
        })
    }
}

impl Primitive for WhitewaterStep {
    fn provides_array_output(&self, port: &str) -> bool {
        OUTPUTS.contains(&port)
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        let population = OUTPUTS.iter().position(|&name| name == port)?;
        self.step.outputs.current().map(|slot| &slot.buffers[population])
    }

    /// Each population output holds the whole pool.
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        OUTPUTS.contains(&port).then(|| match params.get("capacity") {
            Some(ParamValue::Float(v)) => (v.round().max(1.0) as u32).min(MAX_CAPACITY),
            _ => DEFAULT_CAPACITY,
        })
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
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
            ctx.inputs.array("level_set"),
        ) else {
            ctx.error("Whitewater Step: particles, solid, face_u/v/w and level_set must all be wired");
            return;
        };
        let inputs = StepInputs { particles, solid, faces: [face_u, face_v, face_w], level_set };
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
        for (name, count) in ["foam_count", "bubble_count", "spray_count"].into_iter().zip(report.counts) {
            ctx.outputs.set_scalar(name, ParamValue::Float(count as f32));
        }
        ctx.outputs.set_scalar("emitted", ParamValue::Float(report.emitted as f32));
        ctx.outputs.set_scalar("thinned", ParamValue::Float(report.thinned as f32));
        ctx.outputs.set_scalar("pool_full", ParamValue::Float(report.pool_full as f32));
    }
}
