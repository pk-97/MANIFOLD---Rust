//! `node.gpu_flip_step` — one GPU FLIP water step (docs/GPU_FLIP_PRESSURE_SOLVE.md
//! section 1 (the step)): sort, particle distance, particles to faces,
//! extend, forces, solids, water mask from φ, divergence, pressure solve,
//! projection, constraint, extend, density projection, then the particles
//! move, Steps times a tick. One node
//! because no pass has a consumer outside the step and the solve between them
//! is a barriered reduction; the hand kernels live in
//! `shaders/gpu_flip_step.wgsl`, the solver in [`super::gpu_flip_pressure`].
//!
//! Every scratch array is sized from the lattice and the particle slots
//! before any pass is encoded, so no kernel reads `arrayLength`. The face
//! grid output is this node's own storage, exactly one record per padded
//! cell, reallocated when the lattice changes.
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

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use super::gpu_flip_bodies::{BodyPasses, Bodies, REACTION_FLOATS, body_refusal};
use super::gpu_flip_pressure::{MAX_ITERATIONS, PressureSolver, Water, lattice_refusal};
use super::liquid_solid_distance::{SolidDistanceJob, encode_solid_distance};
use super::sort_particles_into_cells::{
    LIQUID_PARTICLE_READ, ParticleSorter, SortJob, SortLabels, float_param, int_param,
};
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid::TICK;
use crate::node_graph::fluid_particles::{FaceSample, FluidParticle};
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::liquid::WATER_DENSITY;
use crate::node_graph::liquid::bodies::{LIQUID_COLLIDER, LIQUID_POSE, LiquidBody, LiquidShape};
use crate::node_graph::liquid::fields::{FieldBinding, LIQUID_FIELD};
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const STEP_SHADER: &str = include_str!("shaders/gpu_flip_step.wgsl");
const NAME: &str = "GPU FLIP Step";

/// Layers of valid faces the step's face grid holds around the water at
/// least: the extension after the projection runs ⌈¾ · travel⌉ + 1 ≥ 2.
pub(crate) const FACE_VALID_LAYERS: u32 = 2;
/// Layers the saved face grid is extended by:
/// one RK3 stage and its sample reach.
pub(crate) const EXTENDED_LAYERS: u32 = 2;
/// Pressure iterations when `iterations` is Auto (0).
pub(crate) const AUTO_PRESSURE_ITERATIONS: u32 = 8;
/// The speed the CFL guard is sized for, m/s.
pub(crate) const DEFAULT_TOP_SPEED: f32 = 20.0;

/// The CFL guard: the farthest one RK3 stage moves a particle, in cells,
/// `top_speed` over one step rounded up. The inputs are f32, so a ratio
/// within 1e-4 of a whole cell is that cell, not the next.
pub(crate) fn travel_cells(top_speed: f32, step_dt: f32, cell_size: f32) -> u32 {
    (f64::from(top_speed) * f64::from(step_dt) / f64::from(cell_size) - 1e-4).ceil().max(1.0) as u32
}

/// Layers the projected faces are extended by: the RK3 stages sample up to ¾
/// of the travel from where a particle started, and a sample reads faces one
/// cell further.
pub(crate) fn band_layers(travel: u32) -> u32 {
    (0.75 * f64::from(travel)).ceil() as u32 + 1
}

/// Bytes of the step's face grid at `cells`: one record per padded cell.
pub(crate) fn face_bytes(cells: [u32; 3]) -> u64 {
    cells.iter().map(|&n| u64::from(n) + 1).product::<u64>() * size_of::<FaceSample>() as u64
}

fn cell_bytes(cells: [u32; 3]) -> u64 {
    cells.iter().map(|&n| u64::from(n)).product::<u64>() * 4
}

/// Bytes the step holds for itself at `cells` with `slots` particle slots,
/// besides the sort's ranges and the solver's scratch: the sorted particles,
/// four cell arrays, the solid corners and five face grids.
#[cfg(any(test, feature = "gpu-proofs"))]
pub(crate) fn scratch_bytes(cells: [u32; 3], slots: u64) -> u64 {
    let corners = cells.iter().map(|&n| u64::from(n) + 1).product::<u64>() * 4;
    slots.max(1) * size_of::<FluidParticle>() as u64 + 4 * cell_bytes(cells) + corners + 5 * face_bytes(cells)
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
}

/// One pass of the step's shader on its own, for the value proofs against
/// the CPU references: `entry` over `threads` threads, `buffers` at their
/// bindings, waited on.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) fn dispatch_pass(device: &GpuDevice, entry: &str, params: &StepParams, buffers: &[(u32, &GpuBuffer)], threads: u64) {
    let pipeline = device.create_compute_pipeline(&step_source(), entry, "node.gpu_flip_step");
    let mut bindings = vec![uniform(params)];
    bindings.extend(buffers.iter().map(|&(binding, b)| buffer(binding, b)));
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
    phi_into_solids: GpuComputePipeline,
    water_from_phi: GpuComputePipeline,
    divergence: GpuComputePipeline,
    distance: GpuComputePipeline,
    subtract: GpuComputePipeline,
    constrain: GpuComputePipeline,
    density: GpuComputePipeline,
    advect: GpuComputePipeline,
}

fn step_source() -> String {
    format!("{LIQUID_POSE}\n{LIQUID_COLLIDER}\n{LIQUID_FIELD}\n{STEP_SHADER}")
}

impl Pipelines {
    fn new(device: &GpuDevice) -> Self {
        let source = step_source();
        let pipe = |entry: &str| device.create_compute_pipeline(&source, entry, "node.gpu_flip_step");
        Self {
            gather: pipe("particles_to_faces"),
            extend: pipe("extend_faces"),
            gravity: pipe("face_gravity"),
            open: pipe("open_fractions"),
            solid_velocity: pipe("solid_face_velocity"),
            phi_into_solids: pipe("phi_into_solids"),
            water_from_phi: pipe("water_from_phi"),
            divergence: pipe("divergence"),
            distance: pipe("particle_distance"),
            subtract: pipe("subtract_pressure"),
            constrain: pipe("constrain_solid_faces"),
            density: pipe("density_source"),
            advect: pipe("faces_to_particles"),
        }
    }
}

/// Scratch for one lattice.
struct LatticeBuffers {
    cells: [u32; 3],
    water: GpuBuffer,
    phi: GpuBuffer,
    rhs: GpuBuffer,
    pressure: GpuBuffer,
    corners: GpuBuffer,
    /// The particles' faces, then the saved (old) faces.
    a: GpuBuffer,
    /// Extension scratch.
    b: GpuBuffer,
    /// The forced, projected, constrained faces.
    f: GpuBuffer,
    /// Open fractions.
    s: GpuBuffer,
    /// The solids' face velocity and friction.
    v: GpuBuffer,
}

fn allocate(device: &GpuDevice, bytes: u64) -> Result<GpuBuffer, String> {
    crate::node_graph::scene_modifier_expand::admit_candidate_bytes(device.modifier_memory_snapshot(), bytes)
        .map_err(|error| error.to_string())
        .and_then(|()| device.try_create_buffer(bytes))
}

impl LatticeBuffers {
    fn new(device: &GpuDevice, cells: [u32; 3]) -> Result<Self, String> {
        let cell = cell_bytes(cells);
        let face = face_bytes(cells);
        let corners = cells.iter().map(|&n| u64::from(n) + 1).product::<u64>() * 4;
        Ok(Self {
            cells,
            water: allocate(device, cell)?,
            phi: allocate(device, cell)?,
            rhs: allocate(device, cell)?,
            pressure: allocate(device, cell)?,
            corners: allocate(device, corners)?,
            a: allocate(device, face)?,
            b: allocate(device, face)?,
            f: allocate(device, face)?,
            s: allocate(device, face)?,
            v: allocate(device, face)?,
        })
    }
}

#[derive(Default)]
pub(crate) struct StepState {
    pipelines: Option<Pipelines>,
    sorter: ParticleSorter,
    solver: PressureSolver,
    solid: Option<GpuComputePipeline>,
    lattice: Option<LatticeBuffers>,
    sorted: Option<GpuBuffer>,
    /// The face grid output: exactly [`face_bytes`] of the current lattice.
    faces: Option<GpuBuffer>,
    /// [`ZERO_BYTES`] zero bytes bound where an optional input is unwired.
    zeros: Option<GpuBuffer>,
    bodies: BodyPasses,
}

/// Zero bytes bound for an unwired input: the uniform-sized arrays, and an
/// empty reaction for every body a liquid holds.
const ZERO_BYTES: u64 = (MAX_FLUID_ROLES * REACTION_FLOATS * 4) as u64;

const SORT_LABELS: SortLabels = SortLabels {
    clear: "gpu_flip.step.sort.clear",
    count: "gpu_flip.step.sort.count",
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
    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(params) }
}

/// `layers` extension passes from `source` into `target`, ping-ponging
/// through `scratch` so the last pass lands in `target`. `source` may be
/// `target` only for an even `layers`: the first pass then writes `scratch`.
fn extend(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    face_groups: [u32; 3],
    [source, target, scratch]: [&GpuBuffer; 3],
    layers: u32,
    label: &str,
) {
    debug_assert!(layers.is_multiple_of(2) || !std::ptr::eq(source, target), "an odd extension would overwrite its source");
    let mut from = source;
    for i in 0..layers {
        let to = if (layers - i) % 2 == 1 { target } else { scratch };
        enc.dispatch_compute(&pipes.extend, &[uniform(params), buffer(3, from), buffer(4, to)], face_groups, label);
        from = to;
    }
}

/// Everything one step reads, resolved before any pass is encoded.
struct Step<'a> {
    params: StepParams,
    particles: &'a GpuBuffer,
    out: &'a GpuBuffer,
    /// Two words a slot: guarded RK3 stages and refused push-outs.
    capped: &'a GpuBuffer,
    count: u32,
    forces: &'a GpuBuffer,
    impulses: &'a GpuBuffer,
    bodies: &'a GpuBuffer,
    shapes: &'a GpuBuffer,
    atlas: &'a GpuBuffer,
    /// What the water has pushed on each body so far this tick, read by the
    /// solid velocity; added to when `dynamic`.
    reaction: &'a GpuBuffer,
    /// The bodies take part in the pressure solve and gather its reaction.
    dynamic: bool,
    pressure_iterations: u32,
    band: u32,
    ghost: bool,
    /// Run the density projection.
    density: bool,
}

impl StepState {
    fn prepare(&mut self, device: &GpuDevice) {
        if self.pipelines.is_none() {
            self.pipelines = Some(Pipelines::new(device));
        }
        self.sorter.prepare(device);
    }

    /// Size every array for `cells` and `slots` before anything is encoded.
    fn reserve(&mut self, device: &GpuDevice, cells: [u32; 3], slots: u64) -> Result<(), String> {
        if self.zeros.is_none() {
            let zeros = device.try_create_buffer_shared(ZERO_BYTES)?;
            zeros.zero_fill();
            self.zeros = Some(zeros);
        }
        self.sorter.reserve_ranges(device, cells)?;
        if self.lattice.as_ref().is_none_or(|l| l.cells != cells) {
            self.lattice = None;
            self.lattice = Some(LatticeBuffers::new(device, cells)?);
        }
        let face = face_bytes(cells);
        if self.faces.as_ref().is_none_or(|faces| faces.size != face) {
            self.faces = None;
            let faces = crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
                device.modifier_memory_snapshot(),
                face,
            )
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
        Ok(())
    }

    fn encode(&mut self, device: &GpuDevice, enc: &mut GpuEncoder, step: &Step<'_>) -> Result<(), String> {
        let (Some(pipes), Some(l), Some(sorted), Some(out_faces)) =
            (self.pipelines.as_ref(), self.lattice.as_ref(), self.sorted.as_ref(), self.faces.as_ref())
        else {
            return Err("the step's storage was not reserved".into());
        };
        let cells = l.cells;
        let p = step.params;
        let capacity = p.capacity;
        self.sorter.encode(
            device,
            enc,
            &SortJob {
                particles: step.particles,
                read: LIQUID_PARTICLE_READ,
                capacity,
                count: step.count,
                bin_min: p.box_min,
                inv_cell: 1.0 / p.cell_size,
                bins: cells,
                sorted: Some(sorted),
                order: None,
            },
            &SORT_LABELS,
        )?;
        let ranges = self.sorter.ranges().ok_or("the cell ranges were not reserved")?;
        let cell_count: u64 = cells.iter().map(|&n| u64::from(n)).product();
        let face_count: u64 = cells.iter().map(|&n| u64::from(n) + 1).product();
        // Copies of the params, so each Bytes binding borrows a value that
        // lives across its dispatch.
        let base = p;
        let ghost = StepParams { ghost: u32::from(step.ghost), ..p };
        let cells_groups = groups(cell_count);
        let face_groups = groups(face_count);

        // The water mask is φ < 0, so φ is built every step.
        let solids = p.body_count > 0;
        enc.dispatch_compute(
            &pipes.distance,
            &[uniform(&base), buffer(1, ranges), buffer(2, sorted), buffer(5, &l.phi)],
            cells_groups,
            "gpu_flip.step.distance",
        );
        enc.dispatch_compute(
            &pipes.gather,
            &[uniform(&base), buffer(1, ranges), buffer(2, sorted), buffer(4, &l.a)],
            face_groups,
            "gpu_flip.step.particles_to_faces",
        );
        // The saved faces: the particles' own, extended so FLIP's change is
        // measured wherever a particle samples.
        extend(enc, pipes, &base, face_groups, [&l.a, &l.a, &l.b], EXTENDED_LAYERS, "gpu_flip.step.extend_old");
        enc.dispatch_compute(
            &pipes.gravity,
            &[uniform(&base), buffer(3, &l.a), buffer(4, &l.f), buffer(12, step.forces), buffer(13, step.impulses)],
            face_groups,
            "gpu_flip.step.forces",
        );
        let corners = cells.map(|n| n + 1);
        // The box walls are in the solid with the bodies, as the engine's
        // inverted domain object is: a body flush with a wall then seals
        // against it, where the body's own lattice distance alone would leave
        // a sub-cell open channel between them. The engine's domain object
        // covers all six faces whichever are open, so this mask stays 63.
        encode_solid_distance(
            &mut self.solid,
            device,
            enc,
            &SolidDistanceJob {
                min: p.box_min,
                cell_size: p.cell_size,
                nodes: corners,
                closed_faces: 63,
                wall_inset: 0,
                body_count: p.body_count,
                rows: p.rows,
                tick_seconds: p.tick_seconds,
                bodies: step.bodies,
                shapes: step.shapes,
                atlas: step.atlas,
                out: &l.corners,
            },
            "gpu_flip.step.solid_distance",
        );
        enc.dispatch_compute(
            &pipes.open,
            &[uniform(&base), buffer(9, &l.corners), buffer(4, &l.s)],
            face_groups,
            "gpu_flip.step.open_fractions",
        );
        enc.dispatch_compute(
            &pipes.solid_velocity,
            &[
                uniform(&base),
                buffer(10, &l.s),
                buffer(4, &l.v),
                buffer(14, step.bodies),
                buffer(15, step.shapes),
                buffer(16, step.atlas),
                buffer(21, step.reaction),
            ],
            face_groups,
            "gpu_flip.step.solid_velocity",
        );
        if solids {
            enc.dispatch_compute(
                &pipes.phi_into_solids,
                &[uniform(&base), buffer(9, &l.corners), buffer(5, &l.phi)],
                cells_groups,
                "gpu_flip.step.phi_into_solids",
            );
        }
        enc.dispatch_compute(
            &pipes.water_from_phi,
            &[uniform(&base), buffer(7, &l.phi), buffer(5, &l.water)],
            cells_groups,
            "gpu_flip.step.water_from_phi",
        );
        enc.dispatch_compute(
            &pipes.divergence,
            &[
                uniform(&base),
                buffer(3, &l.f),
                buffer(5, &l.rhs),
                buffer(6, &l.water),
                buffer(10, &l.s),
                buffer(11, &l.v),
            ],
            cells_groups,
            "gpu_flip.step.divergence",
        );
        let water = Water {
            lattice: cells,
            cell_size: p.cell_size,
            water: &l.water,
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
            water: &l.water,
            open: &l.s,
            solid: &l.v,
            bodies: step.bodies,
        };
        if step.dynamic {
            self.bodies.prepare(device)?;
        }
        let passes = step.dynamic.then_some((&self.bodies, &coupled));
        self.solver.solve(enc, &water, &l.rhs, &l.pressure, step.pressure_iterations, passes)?;
        // φ binds the water array when the ghost rows are off; the pass never reads it then.
        let phi = if step.ghost { &l.phi } else { &l.water };
        let subtract = |enc: &mut GpuEncoder, params: &StepParams, phi: &GpuBuffer, faces: &GpuBuffer, label: &str| {
            enc.dispatch_compute(
                &pipes.subtract,
                &[
                    uniform(params),
                    buffer(20, faces),
                    buffer(10, &l.s),
                    buffer(6, &l.water),
                    buffer(8, &l.pressure),
                    buffer(7, phi),
                ],
                face_groups,
                label,
            );
        };
        subtract(enc, &ghost, phi, &l.f, "gpu_flip.step.project");
        // The engine's finishPressure: the pressure's impulse goes to the
        // bodies and their velocity change to the solid faces, then the
        // constraint's friction is the bodies' too.
        if step.dynamic {
            self.bodies.react(enc, &coupled, &l.pressure, &l.f, step.reaction)?;
        }
        // The engine constrains its velocity and its saved velocity to the
        // solids after the pressure solve, so FLIP's change is measured
        // between two constrained fields.
        for (faces, label) in [(&l.f, "gpu_flip.step.constrain"), (&l.a, "gpu_flip.step.constrain_old")] {
            enc.dispatch_compute(
                &pipes.constrain,
                &[uniform(&base), buffer(20, faces), buffer(10, &l.s), buffer(11, &l.v)],
                face_groups,
                label,
            );
        }
        extend(enc, pipes, &base, face_groups, [&l.f, out_faces, &l.b], step.band, "gpu_flip.step.extend_new");
        // The density projection (module doc): its pressure's gradient is
        // taken off a copy of the new faces in `l.f`, and the move reads the
        // difference as a displacement. Air sits at zero at its centres.
        let spread = if step.density {
            enc.dispatch_compute(
                &pipes.density,
                &[
                    uniform(&base),
                    buffer(1, ranges),
                    buffer(2, sorted),
                    buffer(6, &l.water),
                    buffer(9, &l.corners),
                    buffer(5, &l.rhs),
                ],
                cells_groups,
                "gpu_flip.step.density_source",
            );
            let flat = Water { phi: None, ..water };
            self.solver.solve(enc, &flat, &l.rhs, &l.pressure, step.pressure_iterations, None)?;
            let plain = StepParams { ghost: 0, ..p };
            subtract(enc, &plain, &l.water, &l.f, "gpu_flip.step.density_project");
            extend(enc, pipes, &base, face_groups, [&l.f, &l.f, &l.b], step.band, "gpu_flip.step.extend_spread");
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
            ],
            groups(u64::from(p.particles)),
            "gpu_flip.step.move",
        );
        Ok(())
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

crate::primitive! {
    name: GpuFlipStep,
    type_id: "node.gpu_flip_step",
    purpose: "Advance GPU FLIP water one 60 Hz tick in Steps equal substeps (1 by default); each substep sorts the particles into the lattice's cells, gathers their velocity onto the cell faces, adds gravity and the scene's forces and impulses, makes the water incompressible against the tank walls and the scene's solid bodies (a multigrid-preconditioned pressure solve, the free surface placed where the particles' distance crosses zero), moves crowded particles apart and sparse ones together so the water keeps its volume (a density projection, position only, when Volume Projection is 1), then moves every particle through the new velocity, blending FLIP and PIC by Flip Share, and keeps it out of the solid bodies (a particle a moving body swept over is removed). When dynamic_bodies is above 0, each body that takes a reaction joins the pressure solve with its own velocity, so the water pushes it and it pushes back in the same solve, and the step adds the pressure's and the friction's impulse on every body to the reaction. Outputs the moved particles, the step's face grid (valid at least 2 layers around the water) and the reaction, in place.",
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
        bodies: Array(LiquidBody) optional,
        shapes: Array(LiquidShape) optional,
        atlas: Array(u32) optional,
        body_count: ScalarF32 optional,
        rows: ScalarF32 optional,
        reaction: Array(f32) optional,
        dynamic_bodies: ScalarF32 optional,
        closed_faces: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
        faces: Array(FaceSample),
        reaction_out: Array(f32),
        capped: Array(u32),
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
        int_param!("steps", "Steps", 1.0, 1.0, 64.0),
        float_param!("flip", "Flip Share", 0.95, 0.0, 1.0),
        int_param!("iterations", "Iterations (0 = Auto)", 0.0, 0.0, MAX_ITERATIONS as f32),
        float_param!("top_speed", "Top Speed", DEFAULT_TOP_SPEED, 0.1, 1000.0),
        int_param!("ghost_fluid", "Ghost Fluid", 1.0, 0.0, 1.0),
        int_param!("volume_projection", "Volume Projection", 1.0, 0.0, 1.0),
        int_param!("closed_faces", "Closed Faces", 63.0, 0.0, 63.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Inside node.liquid_state's tick region, once per tick: particles from the state's out, Steps substeps of 1/(60·Steps) s run inside the node, each moving the last one's particles, and the bodies see every substep. The lattice, gravity, the field scalars, forces, impulses, bodies, shapes, atlas, body_count and body_rows (into rows) come from node.gpu_flip_domain; so do dynamic_bodies and reaction, which every substep adds to in place; tick_index from node.liquid_state; count from the fill's live count. Flip Share is the share kept per 1/60 s, so the damping does not change with the step count. The last substep's faces feed node.liquid_state's faces_in, sized exactly to the lattice; out keeps the particles slots. A lattice the device cannot hold, or a side over 1024 cells, is a named error.",
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

/// The solve's iterations: Auto at 0 or below, else the value, refused past
/// the solver's MAX_ITERATIONS.
fn read_iterations(value: f32, auto: u32) -> Result<u32, String> {
    match value.round() {
        v if v <= 0.0 => Ok(auto),
        v if v > MAX_ITERATIONS as f32 => Err(format!("Iterations {v} is past the solver's {MAX_ITERATIONS}")),
        v => Ok(v as u32),
    }
}

impl Primitive for GpuFlipStep {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "faces"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "faces").then_some(self.state.faces.as_ref()).flatten()
    }

    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        match port {
            "out" => inputs.iter().find(|(name, _)| *name == "particles").map(|&(_, n)| n),
            // Provided storage: a one-record hint, sized to the lattice at run time.
            "faces" => Some(1),
            "reaction_out" => inputs.iter().find(|(name, _)| *name == "reaction").map(|&(_, n)| n),
            "capped" => inputs.iter().find(|(name, _)| *name == "particles").map(|&(_, n)| n.saturating_mul(2)),
            _ => None,
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("reaction", "reaction_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.state.prepare(ctx.gpu_encoder().device);
        let Some(lattice) = LiquidLattice::from_wires(ctx, NAME) else {
            return;
        };
        let cells = lattice.cells();
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
        let (Some(particles), Some(out), Some(capped)) =
            (ctx.inputs.array("particles"), ctx.outputs.array("out"), ctx.outputs.array("capped"))
        else {
            return;
        };
        let particle_bytes = size_of::<FluidParticle>() as u64;
        let capacity = (particles.size / particle_bytes).min(u64::from(u32::MAX)) as u32;
        let out_slots = (out.size / particle_bytes).min(u64::from(capacity)) as u32;
        let count = match ctx.inputs.scalar("count") {
            Some(ParamValue::Float(count)) if count.is_finite() => (count.max(0.0) as u32).min(capacity),
            _ => capacity,
        };
        let steps = ctx.scalar_or_param("steps", 1.0).round().clamp(1.0, 64.0);
        let step_dt = (TICK / f64::from(steps)) as f32;
        let flip = ctx.scalar_or_param("flip", 0.95).clamp(0.0, 1.0);
        let flip_per_step = f64::from(flip).powf(60.0 * f64::from(step_dt)) as f32;
        let top_speed = ctx.scalar_or_param("top_speed", DEFAULT_TOP_SPEED);
        if !(top_speed.is_finite() && top_speed > 0.0) {
            ctx.error(format!("{NAME}: Top Speed must be positive, not {top_speed}"));
            return;
        }
        let h = lattice.cell_size();
        let travel = travel_cells(top_speed, step_dt, h);
        let gravity = [("gravity_x", 0.0), ("gravity_y", -9.81), ("gravity_z", 0.0)]
            .map(|(name, default)| ctx.scalar_or_param(name, default));
        let tick_index = ctx.scalar_or_param("tick_index", 0.0).round().max(0.0) as i32;
        let body_count = ctx.scalar_or_param("body_count", 0.0).round().clamp(0.0, MAX_FLUID_ROLES as f32) as i32;
        let rows = ctx.scalar_or_param("rows", 0.0).round().max(0.0) as i32;
        let pressure_iterations = match read_iterations(ctx.scalar_or_param("iterations", 0.0), AUTO_PRESSURE_ITERATIONS) {
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
        let box_min = lattice.box_min();
        if let Err(error) = self.state.reserve(ctx.gpu_encoder().device, cells, u64::from(capacity)) {
            ctx.error(format!(
                "{NAME}: a {}×{}×{} lattice with {capacity} particle slots needs storage the device cannot give: {error}. Lower Resolution.",
                cells[0], cells[1], cells[2]
            ));
            return;
        }
        let zeros = self.state.zeros.clone().expect("zeros prepared");
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
                flip: flip_per_step,
                max_travel: travel as f32,
                box_offset: box_min.iter().fold(0.0_f32, |m, v| m.max(v.abs())),
                ghost: u32::from(ghost),
                particles: out_slots,
                shapes_len,
                // The whole density error each step: projected to rest, no
                // per-step share.
                rate: 1.0 / step_dt,
                closed_faces,
            },
            particles,
            out,
            capped,
            count,
            forces: field.forces.unwrap_or(&zeros),
            impulses: field.impulses.unwrap_or(&zeros),
            bodies,
            shapes,
            atlas,
            reaction,
            dynamic,
            pressure_iterations,
            band: band_layers(travel).max(FACE_VALID_LAYERS),
            ghost,
            density: ctx.scalar_or_param("volume_projection", 1.0) > 0.5,
        };
        let gpu = ctx.gpu_encoder();
        // The first substep reads the tick's particles, every later one the
        // last one's out. The reaction is in place, so the bodies feel every
        // substep.
        for k in 0..steps as i32 {
            step.params.step_in_tick = k;
            step.params.tick_seconds = (k + 1) as f32 * step_dt;
            if k > 0 {
                step.particles = out;
            }
            if let Err(error) = self.state.encode(gpu.device, gpu.native_enc, &step) {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The liquid conformance suite checks codegen bodies for atomics; the
    /// step's hand shader has none, so it is checked here.
    #[test]
    fn step_shader_uses_no_atomics() {
        assert!(!STEP_SHADER.contains("atomic"));
    }

    #[test]
    fn step_shader_validates_with_every_entry() {
        let source = step_source();
        let module = naga::front::wgsl::parse_str(&source).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
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
            "density_source",
            "faces_to_particles",
        ] {
            assert!(entries.contains(&entry), "missing entry {entry}");
        }
    }

    #[test]
    fn step_params_match_the_shader_uniform() {
        assert_eq!(size_of::<StepParams>(), 128);
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

    /// The band covers ¾ of the travel plus the sample's reach, never under
    /// the face grid's guarantee.
    #[test]
    fn band_layers_cover_the_travel() {
        assert_eq!(travel_cells(DEFAULT_TOP_SPEED, 1.0 / 120.0, 0.0625), 3);
        assert_eq!(band_layers(3), 4);
        assert_eq!(band_layers(1), 2);
        for travel in 1..64 {
            assert!(band_layers(travel) >= FACE_VALID_LAYERS);
        }
    }
}
