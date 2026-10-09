//! `node.gpu_flip_domain` — the scene-facing CPU bridge of a GPU FLIP liquid
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` P7a, `docs/GPU_FLIP_PRESSURE_SOLVE.md`):
//! it speaks `node.fluid_surface`'s scene contract (names, types, meanings)
//! and turns it into the fixed-tick clock, the fill's sites, gravity, the
//! Collider roles as body rows, shapes and a distance atlas, and the padded
//! authored lattice descriptor. Separate mesh_min/mesh_nodes outputs carry the native
//! solver and surface grid for solid sampling; the particle frame derives the same grid. Scene
//! forces and impulses reach the water through the shared field lattices of
//! `liquid::fields` (seam P8), sampled over the face grid's box. Paired with
//! a physics world, the scene's Box3D bodies join the water two ways
//! (LIQUID_SOLVER_SEAM_DESIGN.md section 3.3 (Two-way Box3D coupling)): the
//! domain owns the rigid world in-thread and steps it one settled tick at a
//! time with the steps' reaction. Every lattice atom reads the lattice from
//! its wires, so Resolution applies on change. Exempt from the codegen
//! mandate as a CPU bridge (ADDING_PRIMITIVES.md exclusion 3).

use std::borrow::Cow;

use manifold_gpu::{FrameClock, GpuBuffer};
use manifold_physics::FieldValue;

use super::gpu_flip_pressure::{MAX_ITERATIONS, lattice_refusal};
use super::gpu_flip_step::{read_max_iterations, read_sheet_fill_rate, read_solve_level};
use super::liquid_fill::{SITES_PER_CELL, filled_sites, site_range};
use super::matter_domain::closed_faces;
use crate::exec::effect_node::{EffectNodeContext, ParamValues};
use crate::water::fluid::{CoupledRigidFrame, CoupledRigidInputs, FluidDomainLayout, domain_layout};
use crate::water::fluid_role::{FluidRole, MAX_FLUID_ROLES};
use crate::water::liquid::bodies::{BodiesStatus, LiquidBodies, LiquidBody, LiquidShape};
use crate::water::liquid::body_buffers::LiquidBodyBuffers;
use crate::water::liquid::clock::LiquidClock;
use crate::water::liquid::coupling::{DomainWalls, LiquidRigidOwner, PendingTick, REACTION_FLOATS, decode_reaction, takes_reaction};
use {crate::water::liquid::fields, crate::water::liquid::fields::FieldLattice, crate::water::liquid::fields::LiquidFields, crate::water::liquid::fields::LiquidImpulses};
use crate::water::liquid::lattice::{FlipSolverGrid, LiquidLattice};
use crate::water::liquid::tick_samples::TickSamples;
use crate::water::liquid::{EXACT_F32_COUNT, ROLE_PORTS, WATER_DENSITY};
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::water::physics::{RigidImpulseTargets, RigidSceneInputs, RigidSceneObservation, offline_simulation};
use crate::water::physics_events::ResolvedNodeImpulse;
use crate::primitive::Primitive;
use crate::scene::transform::Transform;

manifold_core::testkit_visible! {
/// Everything whose change restarts the liquid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GpuFlipSetup {
    pub(crate) lattice: LiquidLattice,
    pub(crate) pool_sites: u32,
    pub(crate) box_sites: [[u32; 2]; 3],
    /// Particle slots the pool holds; 0 means the fill's count. Sources
    /// emit into the slots past the live particles.
    pub(crate) particle_capacity: u32,
}
}

manifold_core::testkit_visible! {
/// The domain's setup and the layout it came from, computed from params and
/// wires alone: the node and the extent checker both call [`gpu_flip_geometry`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GpuFlipGeometry {
    pub(crate) layout: FluidDomainLayout,
    pub(crate) setup: GpuFlipSetup,
    pub(crate) particles: u64,
    /// The V-cycle level the pressure solves run on; live, so not in the
    /// setup.
    pub(crate) solve_level: usize,
    /// The cap Auto pressure solves converge within; live, so not in the setup.
    pub(crate) max_iterations: u32,
    /// Live sheet seeding rate; changing it does not restart the liquid.
    pub(crate) sheet_fill_rate: f32,
}
}

impl GpuFlipGeometry {
manifold_core::testkit_visible! {
    /// The scalar outputs fixed by the setup, by name.
    pub(crate) fn outputs(&self) -> [(&'static str, f32); 23] {
        let GpuFlipSetup { lattice, pool_sites, box_sites, particle_capacity } = self.setup;
        let h = self.layout.cell_size;
        let surface = lattice.surface();
        let solver = FlipSolverGrid::from_lattice(lattice);
        [
            ("lattice_min_x", lattice.min()[0]),
            ("lattice_min_y", lattice.min()[1]),
            ("lattice_min_z", lattice.min()[2]),
            ("cell_size", lattice.cell_size()),
            ("nodes_x", lattice.nodes()[0] as f32),
            ("nodes_y", lattice.nodes()[1] as f32),
            ("nodes_z", lattice.nodes()[2] as f32),
            ("pool_sites", pool_sites as f32),
            ("box_x0", box_sites[0][0] as f32),
            ("box_x1", box_sites[0][1] as f32),
            ("box_y0", box_sites[1][0] as f32),
            ("box_y1", box_sites[1][1] as f32),
            ("box_z0", box_sites[2][0] as f32),
            ("box_z1", box_sites[2][1] as f32),
            ("particle_mass", (f64::from(WATER_DENSITY) * h * h * h / f64::from(SITES_PER_CELL)) as f32),
            ("particle_capacity", particle_capacity as f32),
            ("mesh_min_x", surface.min()[0]),
            ("mesh_min_y", surface.min()[1]),
            ("mesh_min_z", surface.min()[2]),
            ("mesh_nodes_x", surface.nodes()[0] as f32),
            ("mesh_nodes_y", surface.nodes()[1] as f32),
            ("mesh_nodes_z", surface.nodes()[2] as f32),
            ("mesh_wall_inset", solver.wall_inset()),
        ]
    }
}
}

/// The fill in the layout's half-cell sites: the pool's height and the
/// initial volume's site range per axis (empty without one), refused by name
/// when either does not fit the domain.
fn fill_sites(
    layout: &FluidDomainLayout,
    fill_height: f32,
    initial_volume: Option<Transform>,
) -> Result<(u32, [[u32; 2]; 3]), String> {
    if !fill_height.is_finite() || fill_height < 0.0 || fill_height >= layout.size[1] {
        return Err("GPU FLIP: Initial Fill Height must lie within the domain height".into());
    }
    let min = layout.min.map(f64::from);
    let h = layout.cell_size;
    let pool = site_range(min[1], min[1] + f64::from(fill_height), min[1], h, layout.cells[1])[1];
    let mut sites = [[0u32; 2]; 3];
    if let Some(volume) = initial_volume {
        if volume.billboard || volume.rot_euler.iter().any(|v| !v.is_finite() || v.abs() > 1e-6) {
            return Err("GPU FLIP: the initial volume must be an axis-aligned box; rotation and billboarding are not supported".into());
        }
        if volume.pos.iter().chain(&volume.scale).any(|v| !v.is_finite())
            || volume.scale.iter().any(|v| *v <= 0.0)
            || (0..3).any(|d| {
                let half = volume.scale[d] * 0.5;
                volume.pos[d] - half < layout.min[d] - 1e-4 || volume.pos[d] + half > layout.min[d] + layout.size[d] + 1e-4
            })
        {
            return Err("GPU FLIP: the initial volume must be a finite box fully inside the domain".into());
        }
        for (d, range) in sites.iter_mut().enumerate() {
            let half = f64::from(volume.scale[d]) * 0.5;
            let centre = f64::from(volume.pos[d]);
            *range = site_range(centre - half, centre + half, min[d], h, layout.cells[d]);
        }
    }
    Ok((pool, sites))
}

manifold_core::testkit_visible! {
/// The domain box, lattice and fill from the domain's params and wires
/// (`read` is `scalar_or_param`). Refused by name, in this order: a lattice
/// the pressure solve cannot take, a solve level the lattice lacks, a fill
/// that does not fit the domain, and a fill past the count a wire carries
/// exactly.
pub(crate) fn gpu_flip_geometry(
    read: impl Fn(&str, f32) -> f32,
    domain: Option<Transform>,
    initial_volume: Option<Transform>,
) -> Result<GpuFlipGeometry, String> {
    let resolution = read("resolution", 64.0).round().max(0.0) as u32;
    let layout = domain_layout(domain, read("domain_size", 4.0), resolution)?;
    let solver = FlipSolverGrid::from_lattice(LiquidLattice::from_layout(&layout));
    if let Some(reason) = lattice_refusal(solver.cells()) {
        return Err(format!("GPU FLIP: {reason}. Lower Resolution."));
    }
    let solve_level = read_solve_level(read("solve_level", 0.0), solver.cells()).map_err(|reason| format!("GPU FLIP: {reason}"))?;
    let max_iterations = read_max_iterations(read("max_iterations", MAX_ITERATIONS as f32)).map_err(|reason| format!("GPU FLIP: {reason}"))?;
    let sheet_fill_rate = read_sheet_fill_rate(read("sheet_fill_rate", 0.0), false).map_err(|reason| format!("GPU FLIP: {reason}"))?;
    let (pool_sites, box_sites) = fill_sites(&layout, read("fill_height", 0.4), initial_volume)?;
    let capacity = read("particle_capacity", 0.0);
    if !capacity.is_finite() || capacity < 0.0 {
        return Err("GPU FLIP: Particle Capacity must be 0 (the fill's count) or a positive count".into());
    }
    let particle_capacity = capacity.round() as u64;
    let particles = filled_sites(layout.cells, pool_sites, box_sites).max(particle_capacity);
    if particles > u64::from(EXACT_F32_COUNT) {
        return Err(format!(
            "GPU FLIP: the pool holds {particles} particles, more than the {EXACT_F32_COUNT} a particle count carries exactly. Lower Resolution, Initial Fill Height or Particle Capacity."
        ));
    }
    let setup = GpuFlipSetup {
        lattice: LiquidLattice::from_layout(&layout),
        pool_sites,
        box_sites,
        particle_capacity: particle_capacity as u32,
    };
    Ok(GpuFlipGeometry { layout, setup, particles, solve_level, max_iterations, sheet_fill_rate })
}
}

impl GpuFlipGeometry {
    /// The field lattice over the face grid's box: from its minimum corner
    /// to its far wall faces.
    pub(crate) fn field_lattice(&self) -> FieldLattice {
        let solver = FlipSolverGrid::from_lattice(self.setup.lattice);
        FieldLattice::covering(solver.min(), self.layout.cell_size as f32, solver.nodes())
    }
}

/// Every scalar output, in the order [`GpuFlipDomain::compute`] fills them.
const OUTPUTS: [&str; 57] = [
    "lattice_min_x", "lattice_min_y", "lattice_min_z", "cell_size", "nodes_x", "nodes_y", "nodes_z",
    "closed_faces", "pool_sites", "box_x0", "box_x1", "box_y0", "box_y1", "box_z0", "box_z1",
    "particle_mass", "gravity_x", "gravity", "gravity_z", "ticks", "epoch", "simulation_time",
    "display_time", "target_time", "dropped_seconds", "body_count", "body_rows", "first_tick", "field_nodes_x",
    "field_nodes_y", "field_nodes_z", "field_spacing", "force_lattices", "impulse_tick", "dynamic_bodies",
    "particle_capacity", "region_count", "solve_level",
    "interval_duration",
    "limit_interval",
    "clock_obstacle_count",
    "clock_source_count",
    "live_hit_count", "mesh_min_x", "mesh_min_y", "mesh_min_z", "mesh_nodes_x", "mesh_nodes_y", "mesh_nodes_z",
    "initial_obstacle_speed", "mesh_wall_inset", "max_iterations", "display_cursor", "sheet_fill_rate",
    "handover_position", "handover_velocity", "handover_rotation"];
const TICKS: usize = 19;
const IMPULSE_TICK: usize = 33;
const INITIAL_OBSTACLE_SPEED: usize = 49;
const _: () = assert!(matches!(OUTPUTS[TICKS].as_bytes(), b"ticks"));
const _: () = assert!(matches!(OUTPUTS[IMPULSE_TICK].as_bytes(), b"impulse_tick"));
const _: () = assert!(matches!(OUTPUTS[INITIAL_OBSTACLE_SPEED].as_bytes(), b"initial_obstacle_speed"));

/// The coupled half of the domain: the paired physics world's latest
/// observation, and the rigid owner built from it.
#[derive(Default)]
pub struct Coupling {
    mode: bool,
    observation: Option<RigidSceneObservation>,
    /// The paired world's authored scene at each tick's start: Box3D steps a
    /// tick toward the scene at its end, so animated bodies move per tick.
    scenes: TickSamples<RigidSceneInputs>,
    colliders: RigidImpulseTargets,
    error: Option<String>,
    previous_reset: Option<f32>,
    owner: Option<LiquidRigidOwner>,
    /// This frame's domain box and closed faces: the bodies' walls.
    walls: DomainWalls,
    /// Rows before the pending tick's first coupled body's (its Collider
    /// roles'), so its reaction decodes against the bodies it ran with.
    offset: Option<usize>,
    /// The owner was built since the clock last restarted: the next frame
    /// restarts the liquid with it.
    owner_fresh: bool,
    epochs: u64,
    /// This frame's compute failed; nothing is published to the pair.
    failed: bool,
    /// This frame runs several coupled ticks, exchanging with Box3D
    /// between them.
    exchange: Option<Exchange>,
    /// A host step failed; the next frame reports it and restarts the pair.
    host_error: Option<String>,
}

impl Coupling {
    fn reset(&mut self) {
        let (mode, epochs) = (self.mode, self.epochs);
        *self = Self { mode, epochs, ..Self::default() };
    }
}

/// A frame of several coupled ticks: the region syncs before each
/// later tick, and the domain settles the tick before it there.
#[derive(Clone)]
struct Exchange {
    frame: manifold_physics::clock::ClockFrame,
    ticks: u32,
    /// The frame's first tick; later ticks differ only in `tick`.
    pending: PendingTick,
    offset: usize,
}

/// The reaction's floats, once their last GPU writer has retired.
fn reaction_floats(buffer: Option<&GpuBuffer>) -> Option<&[f32]> {
    let buffer = buffer?;
    let floats = buffer.mapped_ptr()?.cast::<f32>().cast_const();
    // SAFETY: shared, 4-byte aligned storage whose last GPU writer has
    // retired (the caller checked the frame clock or waited on the GPU).
    Some(unsafe { std::slice::from_raw_parts(floats, (buffer.size / 4) as usize) })
}

crate::primitive! {
    name: GpuFlipDomain,
    type_id: "node.gpu_flip_domain",
    purpose: "Define a GPU FLIP liquid domain with node.fluid_surface's scene contract: the axis-aligned domain box, resolution, initial fill height and box, gravity, the scene's acceleration field and impulses, simulation speed and reset. Outputs this frame's fixed 60 Hz ticks, the epoch and the display clock, the fill's half-cell sites and particle mass, gravity, the scene's forces and impulses on coarse field lattices over the box, and the padded lattice with its Closed Faces mask (bit 2d the low face of axis d, bit 2d + 1 the high one) for the step, the solid distance and the particle frame. Each face of the tank is closed unless its Closed param is off; an open face drains the water that reaches it. A hit fired while the liquid is held (paused, Speed 0) is discarded, never replayed. Up to 64 Collider roles become one body row per collider per tick of this frame (its pose at the tick's start and its motion over the tick), a shape per collider and their distance lattices packed in one half-precision atlas; the water flows around them. Paired with a physics world, its bodies join the water two ways: the domain steps the world one settled 1/60 s tick at a time, its bodies' rows follow the collider rows, dynamic_bodies counts the ones the water pushes, and the reaction the steps add up reaches each body as one impulse per tick. Live, the pair runs at most one tick per frame and holds while that tick's reaction is still on the GPU. Inflow and Outflow roles become region rows (regions, region_count) beside the body rows, sharing the shapes and atlas: an inflow emits water at its velocity into its volume each substep, an outflow removes the water inside it. Particle Capacity sizes the particle pool sources emit into (0 = the fill's count); a full pool stops emitting and shows in the stats. Fill roles are refused by name.",
    inputs: {
        domain: Transform optional,
        initial_volume: Transform optional,
        acceleration_field: VectorField optional,
        resolution: ScalarF32 optional,
        domain_size: ScalarF32 optional,
        fill_height: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        speed: ScalarF32 optional,
        reset: ScalarF32 optional,
        role_0: FluidRole optional,
        role_1: FluidRole optional,
        role_2: FluidRole optional,
        role_3: FluidRole optional,
        role_4: FluidRole optional,
        role_5: FluidRole optional,
        role_6: FluidRole optional,
        role_7: FluidRole optional,
        role_8: FluidRole optional,
        role_9: FluidRole optional,
        role_10: FluidRole optional,
        role_11: FluidRole optional,
        role_12: FluidRole optional,
        role_13: FluidRole optional,
        role_14: FluidRole optional,
        role_15: FluidRole optional,
        role_16: FluidRole optional,
        role_17: FluidRole optional,
        role_18: FluidRole optional,
        role_19: FluidRole optional,
        role_20: FluidRole optional,
        role_21: FluidRole optional,
        role_22: FluidRole optional,
        role_23: FluidRole optional,
        role_24: FluidRole optional,
        role_25: FluidRole optional,
        role_26: FluidRole optional,
        role_27: FluidRole optional,
        role_28: FluidRole optional,
        role_29: FluidRole optional,
        role_30: FluidRole optional,
        role_31: FluidRole optional,
        role_32: FluidRole optional,
        role_33: FluidRole optional,
        role_34: FluidRole optional,
        role_35: FluidRole optional,
        role_36: FluidRole optional,
        role_37: FluidRole optional,
        role_38: FluidRole optional,
        role_39: FluidRole optional,
        role_40: FluidRole optional,
        role_41: FluidRole optional,
        role_42: FluidRole optional,
        role_43: FluidRole optional,
        role_44: FluidRole optional,
        role_45: FluidRole optional,
        role_46: FluidRole optional,
        role_47: FluidRole optional,
        role_48: FluidRole optional,
        role_49: FluidRole optional,
        role_50: FluidRole optional,
        role_51: FluidRole optional,
        role_52: FluidRole optional,
        role_53: FluidRole optional,
        role_54: FluidRole optional,
        role_55: FluidRole optional,
        role_56: FluidRole optional,
        role_57: FluidRole optional,
        role_58: FluidRole optional,
        role_59: FluidRole optional,
        role_60: FluidRole optional,
        role_61: FluidRole optional,
        role_62: FluidRole optional,
        role_63: FluidRole optional,
    },
    outputs: {
        lattice_min_x: ScalarF32, lattice_min_y: ScalarF32, lattice_min_z: ScalarF32,
        mesh_min_x: ScalarF32, mesh_min_y: ScalarF32, mesh_min_z: ScalarF32,
        mesh_nodes_x: ScalarF32, mesh_nodes_y: ScalarF32, mesh_nodes_z: ScalarF32,
        cell_size: ScalarF32,
        nodes_x: ScalarF32, nodes_y: ScalarF32, nodes_z: ScalarF32,
        closed_faces: ScalarF32,
        pool_sites: ScalarF32,
        box_x0: ScalarF32, box_x1: ScalarF32,
        box_y0: ScalarF32, box_y1: ScalarF32,
        box_z0: ScalarF32, box_z1: ScalarF32,
        particle_mass: ScalarF32,
        gravity_x: ScalarF32, gravity: ScalarF32, gravity_z: ScalarF32,
        ticks: ScalarF32,
        epoch: ScalarF32,
        simulation_time: ScalarF32,
    interval_duration: ScalarF32,
    limit_interval: ScalarF32,
    clock_obstacle_count: ScalarF32, clock_source_count: ScalarF32,
    initial_obstacle_speed: ScalarF32,
    clock_obstacles: Array(f32), clock_sources: Array(f32),
    live_hits: Array(f32), live_hit_count: ScalarF32,
        target_time: ScalarF32,
        display_time: ScalarF32,
        display_cursor: ScalarF32,
        dropped_seconds: ScalarF32,
        body_count: ScalarF32, body_rows: ScalarF32, dynamic_bodies: ScalarF32,
        handover_position: ScalarF32, handover_velocity: ScalarF32, handover_rotation: ScalarF32,
        first_tick: ScalarF32,
        field_nodes_x: ScalarF32, field_nodes_y: ScalarF32, field_nodes_z: ScalarF32,
        field_spacing: ScalarF32,
        force_lattices: ScalarF32,
        impulse_tick: ScalarF32,
        particle_capacity: ScalarF32,
        region_count: ScalarF32,
        solve_level: ScalarF32,
        max_iterations: ScalarF32,
        sheet_fill_rate: ScalarF32,
        mesh_wall_inset: ScalarF32,
        bodies: Array(LiquidBody), regions: Array(LiquidBody), shapes: Array(LiquidShape), atlas: Array(u32),
        reaction: Array(f32),
        contacts: Array(f32),
        forces: Array(f32), impulses: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("resolution"), label: "Resolution", ty: ParamType::Int, default: ParamValue::Float(64.0), range: Some((8.0, 512.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("solve_level"), label: "Solve Level", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("max_iterations"), label: "Max Iterations", ty: ParamType::Int, default: ParamValue::Float(MAX_ITERATIONS as f32), range: Some((1.0, MAX_ITERATIONS as f32)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("sheet_fill_rate"), label: "Sheet Fill Rate", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("domain_size"), label: "Domain Size", ty: ParamType::Float, default: ParamValue::Float(4.0), range: Some((0.5, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_height"), label: "Initial Fill Height", ty: ParamType::Float, default: ParamValue::Float(0.4), range: Some((0.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("speed"), label: "Simulation Speed", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("reset"), label: "Reset", ty: ParamType::Trigger, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_x"), label: "Closed −X", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_x"), label: "Closed +X", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_y"), label: "Closed Bottom", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_y"), label: "Closed Top", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_z"), label: "Closed −Z", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_z"), label: "Closed +Z", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("particle_capacity"), label: "Particle Capacity", ty: ParamType::Int, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "The GPU FLIP group's source of truth: ticks and epoch into node.liquid_state (the tick region's clock owner), the fill sites and the padded lattice into node.liquid_fill, gravity, forces, impulses, the field scalars (field_nodes_x/y/z, field_spacing, force_lattices, first_tick, impulse_tick), bodies, regions, region_count, shapes, atlas, dynamic_bodies and the padded lattice into every node.gpu_flip_step, reaction into the first step of the tick (each later step takes the one before's reaction_out), and the padded lattice with bodies, shapes and atlas into node.liquid_solid_distance, node.liquid_frame and node.face_sample_component; simulation_time, display_time and epoch into node.liquid_frame; particle_mass into node.liquid_stats; particle_capacity into node.liquid_fill. The domain box, Resolution and fill restart the liquid; gravity and Simulation Speed are live. Live runs at most two fixed Sim Rate intervals per display frame and reports dropped time; export runs every interval.",
    examples: [],
    picker: { label: "GPU FLIP Domain", category: Atom },
    summary: "Sets up a GPU FLIP liquid: its box, resolution, starting fill, gravity and speed.",
    category: Particles3D,
    role: Source,
    aliases: ["gpu flip", "gpu water", "liquid domain"],
    boundary_reason: NonGpu,
    extra_fields: {
        clock: LiquidClock = LiquidClock::default(),
        scheduled_frame: Option<manifold_physics::clock::ClockFrame> = None,
        setup: Option<GpuFlipSetup> = None,
        published: Option<[f32; OUTPUTS.len()]> = None,
        bodies: LiquidBodies = LiquidBodies::with_regions(),
        body_buffers: LiquidBodyBuffers = LiquidBodyBuffers::default(),
        role_pending: bool = false,
        body_rows: f32 = 0.0,
        rows_fresh: bool = false,
        coupled: Coupling = Coupling::default(),
        reaction: Option<GpuBuffer> = None,
        impulses: LiquidImpulses = LiquidImpulses::default(),
        fields: LiquidFields = LiquidFields::default(),
        acceleration: Option<FieldValue> = None,
        holding: bool = false,
        // The obstacle CFL bound of a later tick of this frame, prepared by
        // the host step before it (see `substep_clock_interval`).
        later_obstacle: [(&'static str, f32); 1] = [(OUTPUTS[INITIAL_OBSTACLE_SPEED], -1.0)],
        later_obstacle_tick: Option<u32> = None,
    },
}

/// A later tick whose bodies no host step prepared has no obstacle bound.
const NO_OBSTACLE_BOUND: &[(&str, f32)] = &[(OUTPUTS[INITIAL_OBSTACLE_SPEED], -1.0)];

impl Primitive for GpuFlipDomain {
    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "bodies" | "contacts" | "regions" | "shapes" | "atlas" | "reaction" | "forces" | "impulses"| "clock_obstacles"
                | "clock_sources"
                | "live_hits")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "reaction" => return self.reaction.as_ref(),
            "forces" => return self.fields.forces_buffer(),
            "impulses" => return self.fields.impulses_buffer(),
            "live_hits" => return self.fields.live_hits_buffer(),
            _ => {}
        }
        self.body_buffers.output(port)
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        matches!(port_name, "bodies" | "contacts" | "regions" | "shapes" | "atlas" | "reaction" | "forces" | "impulses"| "clock_obstacles"
                | "clock_sources"
                | "live_hits").then_some(1)
    }

    fn warmup_pending(&self) -> bool {
        self.role_pending
    }

    /// The frame's outputs carry the first tick's obstacle bound. A later
    /// tick gets its own once the host step has prepared its bodies, and
    /// none before, so a step never pairs one tick's bodies with another's.
    fn substep_clock_interval(&self, iteration: u32) -> Option<crate::exec::substeps::SubstepClockOutput<'_>> {
        let frame = self.scheduled_frame.as_ref()?;
        frame.interval(u64::from(iteration)).map(|interval| {
            let mut output = crate::exec::substeps::SubstepClockOutput::single("interval_duration", interval, iteration, frame.ticks);
            if iteration > 0 {
                output.scalars = if self.later_obstacle_tick == Some(iteration) { &self.later_obstacle } else { NO_OBSTACLE_BOUND };
            }
            output
        })
    }

    /// Coupled frames sync before each later tick of the frame.
    fn substep_host_sync(&self, iteration: u32) -> bool {
        self.coupled.exchange.as_ref().is_some_and(|x| iteration < x.ticks)
    }

    /// Between two ticks of one frame: settle the tick the GPU just
    /// finished, step Box3D over it, and hand the next tick the bodies' new
    /// state and a cleared reaction.
    fn substep_host_step(
        &mut self,
        iteration: u32,
        _gpu: Option<&mut crate::gpu::gpu_encoder::GpuEncoder<'_>>,
    ) -> Result<(), String> {
        let Some(exchange) = self.coupled.exchange.clone() else { return Ok(()) };
        let duration = exchange.frame.interval(u64::from(iteration)).map(|interval| interval.duration().0 as f32);
        let result = self.exchange_tick(iteration, exchange);
        if result.is_ok() && let Some(duration) = duration {
            // exchange_tick prepared this tick's clock vertices.
            self.later_obstacle[0].1 = self.bodies.initial_clock_obstacle_speed(duration);
            self.later_obstacle_tick = Some(iteration);
        }
        if let Err(error) = &result {
            // The pair restarts next frame with a fresh rigid owner.
            self.coupled.exchange = None;
            self.coupled.owner = None;
            self.coupled.host_error = Some(error.clone());
        }
        result
    }

    /// The liquid restarts in a new epoch, and a coupled pair together with
    /// a fresh rigid owner.
    fn clear_state(&mut self) {
        self.coupled.reset();
        self.clock.restart();
    }

    fn request_physics_samples(&mut self, from: f64, until: f64, out: &mut Vec<f64>) {
        self.fields.request_samples(&self.clock, from, until, out);
        self.bodies.request_samples(&self.clock, from, until, out);
        if self.coupled.mode {
            self.coupled.scenes.request(&self.clock, from, until, out);
        }
    }

    fn set_coupled_physics(&mut self, enabled: bool) {
        if self.coupled.mode != enabled {
            self.coupled.mode = enabled;
            self.clear_state();
        }
    }

    fn set_coupled_rigid_inputs(
        &mut self,
        observation: Option<&RigidSceneObservation>,
        colliders: RigidImpulseTargets,
        error: Option<&str>,
    ) {
        // A replay sample records the scene at a tick's start; a pending one
        // records nothing, and `run` closes the sample.
        if crate::water::physics::authored_sample_only() {
            if let Some(observation) = observation.filter(|_| error.is_none()) {
                self.coupled.scenes.observe(observation.transport.0, Some(&observation.inputs));
            }
            return;
        }
        self.set_coupled_physics(true);
        self.coupled.observation = observation.cloned();
        self.coupled.colliders = colliders;
        self.coupled.error = error.map(str::to_owned);
    }

    fn coupled_rigid_frame(&self) -> Option<&CoupledRigidFrame> {
        let coupled = &self.coupled;
        if !coupled.mode || self.role_pending || coupled.failed || coupled.observation.is_none() || coupled.error.is_some() {
            return None;
        }
        coupled.owner.as_ref().map(LiquidRigidOwner::frame)
    }

    // While an input is pending the liquid holds its last good frame.
    fn runs_with_pending_inputs(&self) -> bool {
        true
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let mut roles: [Option<FluidRole>; MAX_FLUID_ROLES] = std::array::from_fn(|_| None);
        let role_pending = crate::water::liquid::read_roles(&ctx.inputs, &ROLE_PORTS, &mut roles);
        // A physics sample reads the force field and the roles at a tick's
        // start; it never advances time.
        if crate::water::physics::authored_sample_only() {
            self.clock.observe_speed(ctx.time.seconds.0, ctx.scalar_or_param("speed", 1.0));
            let field = ctx.inputs.vector_field("acceleration_field");
            self.fields.observe_sample(ctx.time.seconds.0, field.as_ref());
            self.bodies.observe_sample(ctx.time.seconds.0, (!role_pending).then_some(&roles[..]));
            self.coupled.scenes.observe(ctx.time.seconds.0, None);
            return;
        }
        self.role_pending = ctx.inputs.any_pending() || role_pending;
        self.acceleration = ctx.inputs.vector_field("acceleration_field");
        if ctx.inputs.slot("acceleration_field").is_some() {
            self.role_pending |= self.acceleration.is_none();
        }
        // Every output is published every frame, so no consumer reads a slot
        // this node left unwritten. While a role or the field is still being
        // prepared, or on an error, the liquid holds: the last good outputs
        // repeat with zero ticks and no impulse. Before the first good frame
        // there is nothing to hold, so the outputs are declared pending.
        self.coupled.exchange = None;
        self.scheduled_frame = None;
        self.later_obstacle_tick = None;
        let computed = if self.role_pending { Ok(None) } else { self.compute(ctx, &roles) };
        let held = || {
            let mut held = self.published.unwrap_or([0.0; OUTPUTS.len()]);
            held[TICKS] = 0.0;
            held[IMPULSE_TICK] = -1.0;
            held[INITIAL_OBSTACLE_SPEED] = -1.0;
            held
        };
        self.coupled.failed = false;
        let (values, fresh) = match computed {
            Ok(Some(values)) => (values, true),
            Ok(None) => {
                self.role_pending = true;
                (held(), false)
            }
            Err(error) => {
                self.coupled.failed = true;
                ctx.error(error);
                (held(), false)
            }
        };
        self.holding = !fresh;
        if fresh {
            self.published = Some(values);
        }
        if self.published.is_none() {
            ctx.mark_outputs_pending();
        }
        for (name, value) in OUTPUTS.iter().zip(values) {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
        if let Some(gpu) = ctx.gpu.as_deref_mut() {
            // Sized for every body a liquid holds, so a pending reaction never
            // moves when roles come and go.
            let reaction = self.reaction.get_or_insert_with(|| {
                let bytes = MAX_FLUID_ROLES * REACTION_FLOATS * 4;
                let buffer = gpu.device.create_buffer_shared(bytes as u64);
                // SAFETY: new shared buffer, not yet visible to the GPU.
                unsafe { buffer.write(0, &vec![0u8; bytes]) };
                buffer
            });
            // A coupled tick sums its reaction from zero; the floats stay put
            // until the next tick so the owner reads them once the frame
            // retires.
            if values[TICKS] > 0.0 && self.coupled.owner.is_some() {
                gpu.native_enc.clear_buffer(reaction);
            }
            self.body_buffers.upload(gpu, &self.bodies, fresh && self.rows_fresh, "node.gpu_flip_domain.bodies");
        }
        let uploaded = match ctx.gpu.as_deref_mut().map(|gpu| self.fields.upload(gpu, offline_simulation())) {
            Some(Ok(())) => true,
            Some(Err(error)) => {
                ctx.error(error);
                false
            }
            None => false,
        };
        // A hit is "applied" only once its tick's fields are on the GPU; a
        // frame that held or failed discards its hits with a receipt.
        if fresh && uploaded {
            self.impulses.commit_frame();
        } else {
            self.impulses.abandon_frame();
        }
    }

    fn physics_impulse_epoch(&self) -> Option<u64> {
        self.impulses.epoch()
    }

    /// A hit fired this frame is stamped at the liquid's simulated time, so
    /// it lands on the first tick of the next frame.
    fn physics_impulse_stamp(
        &self,
        transport: manifold_core::Seconds,
        sequence: u64,
    ) -> Result<manifold_physics::input::EventStamp, String> {
        if self.holding {
            return Err("GPU FLIP impulses: cannot capture an impulse while the liquid is pending or failed".into());
        }
        let mut stamp = self.impulses.stamp(transport.0, sequence)?;
        stamp.time = manifold_core::Seconds(self.clock.simulation_at(transport.0));
        Ok(stamp)
    }

    fn enqueue_physics_impulse(
        &mut self,
        stamp: manifold_physics::input::EventStamp,
        impulse: ResolvedNodeImpulse,
    ) -> Result<manifold_physics::TickStamp, String> {
        self.impulses.enqueue(stamp, impulse, self.coupled.owner.as_mut())
    }

    fn drain_physics_impulses(
        &mut self,
        consume: &mut dyn FnMut(manifold_physics::input::AppliedEvent<ResolvedNodeImpulse>),
    ) {
        self.impulses.drain_applied(consume, self.coupled.owner.as_mut());
    }

    fn drain_discarded_impulses(&mut self, consume: &mut dyn FnMut(manifold_physics::input::EventStamp)) {
        self.impulses.drain_discarded(consume);
    }
}

impl GpuFlipDomain {
    /// This frame's outputs, or None while a collider's distance lattice is
    /// still building (the liquid holds and the clock does not advance).
    fn compute(
        &mut self,
        ctx: &EffectNodeContext<'_, '_>,
        roles: &[Option<FluidRole>],
    ) -> Result<Option<[f32; OUTPUTS.len()]>, String> {
        let geometry = gpu_flip_geometry(
            |name, default| ctx.scalar_or_param(name, default),
            ctx.inputs.transform("domain"),
            ctx.inputs.transform("initial_volume"),
        )?;
        let speed = ctx.scalar_or_param("speed", 1.0);
        let gravity = [
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
        ];
        if !speed.is_finite() || !(0.0..=4.0).contains(&speed) || gravity.iter().any(|g| !g.is_finite()) {
            return Err("GPU FLIP: Simulation Speed must be between 0 and 4, and gravity must be finite".into());
        }
        let offline = offline_simulation();
        if let Some(error) = self.coupled.host_error.take() {
            return Err(error);
        }
        let clock = if self.coupled.mode { ctx.gpu.as_deref().and_then(|gpu| gpu.device.frame_clock()) } else { None };
        self.coupled.walls = DomainWalls::of(&geometry.layout, closed_faces(ctx.params));
        if self.coupled.mode && !self.observe_rigid(ctx.time.seconds.0, speed)? {
            return Ok(None);
        }
        let coupled_geometries = self.coupled.owner.as_ref().map_or(&[][..], LiquidRigidOwner::geometries);
        if self.bodies.prepare(roles, coupled_geometries, geometry.setup.lattice.cell_size(), offline)?
            == BodiesStatus::Pending
        {
            return Ok(None);
        }
        let restart = self.setup != Some(geometry.setup) || self.coupled.owner_fresh;
        // Finish the prior reaction before reusing its buffer. Live and
        // offline share per-tick exchanges; only the clock limits live work.
        if !restart
            && let (Some(owner), Some(_)) = (&mut self.coupled.owner, &self.coupled.observation)
        {
            let reaction = self.reaction.as_ref();
            let offset = self.coupled.offset;
            // This frame's clear is encoded after this read.
            let settled = owner.settle_ready(
                self.coupled.scenes.endpoint(owner.completed() + 1),
                |stamp| clock.as_ref().is_none_or(|clock| {
                    clock.is_complete(stamp) || (clock.wait(stamp) && clock.is_complete(stamp))
                }),
                |_, rows, impulses| {
                    let offset = offset.ok_or("GPU FLIP coupling: the pending tick has no body offset")?;
                    decode_reaction(offset, rows, reaction_floats(reaction), impulses)
                },
            );
            if let Err(error) = settled {
                // A dead reaction or a failed step: the pair restarts
                // with a fresh rigid owner, the error reported.
                self.coupled.owner = None;
                return Err(error);
            }
            if let Some(start) = self.coupled.scenes.reanchored_start(owner.completed()) {
                owner.reanchor_start(start)?;
            }
        }
        self.setup = Some(geometry.setup);
        let frame = self.clock.advance(
            ctx.time.seconds.0,
            crate::water::physics::simulation_interval(),
            speed,
            ctx.scalar_or_param("reset", 0.0),
            restart,
            offline,
        );
        self.scheduled_frame = Some(frame.clone());
        if frame.numerical_error {
            crate::water::physics_metrics::record_simulation(0.0, 0.0, false, true);
        }
        if frame.restarted && self.coupled.owner.is_some() && !self.coupled.owner_fresh {
            self.rebuild_owner()?;
        }
        self.coupled.owner_fresh = false;
        // After reconciliation, so a restart never reports the old owner's completion.
        crate::water::physics_metrics::record_clock(
            &self.clock,
            &frame,
            self.coupled.owner.as_ref().map(LiquidRigidOwner::completed),
        );
        if let Some(owner) = &self.coupled.owner {
            let scene = self.coupled.observation.as_ref().map(|observation| &observation.inputs);
            self.coupled.scenes.settle(&self.clock, &frame, scene);
            self.coupled.scenes.prune_before(owner.completed());
        }
        self.impulses.observe_frame(ctx.time.seconds.0, &frame)?;
        self.bodies.settle(roles, &self.clock, &frame);
        let first_tick = fields::first_tick(&frame);
        // A restart runs no tick yet still publishes the first tick's rows: the
        // fill seeds around the bodies' starting poses. Otherwise a frame
        // without ticks keeps the last rows as the bodies' poses.
        let row_ticks = if frame.restarted { frame.ticks.max(1) } else { frame.ticks };
        self.rows_fresh = row_ticks > 0;
        let coupled_rows = self.coupled.owner.as_ref().map_or(&[][..], LiquidRigidOwner::rows);
        if row_ticks > 0 {
            self.body_rows = self.bodies.rows(first_tick, frame.ticks, coupled_rows)?.len() as f32;
            self.bodies.set_contacts(self.coupled.owner.as_ref().map_or(&[][..], LiquidRigidOwner::contacts))?;
        }
        let dynamic_bodies = coupled_rows.iter().filter(|row| takes_reaction(row)).count();
        self.bodies
            .prepare_clock_vertices(geometry.layout.min, geometry.layout.size, 0);
        let mut display_time = frame.display_time;
        if let Some(owner) = &mut self.coupled.owner {
            if frame.ticks > 0 {
                if first_tick != owner.completed() {
                    return Err(format!(
                        "GPU FLIP coupling: water ticks {first_tick}+{} do not follow rigid tick {}",
                        frame.ticks,
                        owner.completed()
                    ));
                }
                let pending = PendingTick { tick: first_tick, stamp: clock.as_ref().map_or(0, FrameClock::stamp) ,
                    interval: frame.interval(0).expect("pending interval"),
                    offline: frame.offline};
                let offset = self.bodies.count() - owner.rows().len();
                owner.set_pending(pending);
                self.coupled.offset = Some(offset);
                if frame.ticks > 1 {
                    self.coupled.exchange = Some(Exchange { frame: frame.clone(), ticks: frame.ticks, pending, offset });
                }
            }
            // The water is shown at the tick Box3D has settled, with the
            // bodies' accepted frame, which the scene takes before the region
            // runs this frame's ticks.
            display_time = owner.completed_time().0;
        }
        self.fields.split_live_hits();
        let field = self.fields.prepare(
            geometry.field_lattice(),
            self.acceleration.as_ref(),
            &self.clock,
            &frame,
            &self.impulses,
        )?;
        let handover = self.coupled.owner.as_ref().map_or_else(Default::default, LiquidRigidOwner::handover);
        let per_frame = [
            ("closed_faces", closed_faces(ctx.params) as f32),
            ("solve_level", geometry.solve_level as f32),
            ("max_iterations", geometry.max_iterations as f32),
            ("sheet_fill_rate", geometry.sheet_fill_rate),
            ("gravity_x", gravity[0]),
            ("gravity", gravity[1]),
            ("gravity_z", gravity[2]),
            ("ticks", frame.ticks as f32),
            ("epoch", frame.epoch as f32),
            ("simulation_time", frame.simulation_time as f32),
            ("interval_duration", frame.duration().0 as f32),
            // Export steps each interval, so its limit measures the step (0).
            (
                "limit_interval",
                // Compatibility output for saved graphs. Each solver call is
                // now exactly one accepted interval, so no span override is needed.
                0.0,
            ),
            (
                "clock_obstacle_count",
                self.bodies.clock_obstacles().len() as f32,
            ),
            (
                "clock_source_count",
                self.bodies.clock_sources().len() as f32,
            ),
            ("live_hit_count", self.fields.live_hit_count() as f32),
            ("target_time", frame.target_time as f32),
            ("display_time", display_time as f32),
            // Live uncoupled water presents behind at liquid_frame's cursor;
            // export and coupled water present exactly (display history D3).
            ("display_cursor", if offline || self.coupled.owner.is_some() { 0.0 } else { 1.0 }),
            ("dropped_seconds", frame.dropped_seconds as f32),
            ("body_count", self.bodies.count() as f32),
            ("region_count", self.bodies.region_count() as f32),
            ("body_rows", self.body_rows),
            ("dynamic_bodies", dynamic_bodies as f32),
            // How far Box3D ended the last settled tick from the coupled motion
            // law, worst body (m, m/s, rad; D18).
            ("handover_position", handover.position),
            ("handover_velocity", handover.velocity),
            ("handover_rotation", handover.rotation),
            ("first_tick", first_tick as f32),
            // Prepared first rows, before the first interval's cleared reaction
            // changes. Later intervals require their own freshness proof.
            ("initial_obstacle_speed", frame.interval(0).map_or(-1.0, |interval| {
                self.bodies.initial_clock_obstacle_speed(interval.duration().0 as f32)
            })),
        ];
        let mut values = [0.0; OUTPUTS.len()];
        let mut written = 0u64;
        for (name, value) in geometry.outputs().into_iter().chain(per_frame).chain(field.outputs()) {
            let slot = OUTPUTS.iter().position(|output| *output == name).expect("every output has a slot");
            values[slot] = value;
            written |= 1 << slot;
        }
        debug_assert_eq!(written, (1 << OUTPUTS.len()) - 1, "every output written once");
        Ok(Some(values))
    }

    /// Take the paired world's observation for this frame: check it shares
    /// the water's clock, and build a new rigid owner (restarting the pair)
    /// on its reset or a change of bodies. False while the world's inputs are
    /// still pending.
    fn observe_rigid(&mut self, transport: f64, speed: f32) -> Result<bool, String> {
        let Coupling { observation, colliders, error, previous_reset, owner, owner_fresh, epochs, walls, .. } = &mut self.coupled;
        if let Some(error) = error {
            return Err(error.clone());
        }
        let Some(observation) = observation.as_ref() else { return Ok(false) };
        if speed != observation.speed {
            return Err(format!(
                "GPU FLIP coupling: water and rigid bodies require the same Simulation Speed and shared World control (water {speed}, rigid {})",
                observation.speed
            ));
        }
        if (observation.transport.0 - transport).abs() > 1e-9 {
            return Err("GPU FLIP coupling: rigid observation transport does not match water transport".into());
        }
        CoupledRigidInputs { scene: &observation.inputs, colliders: *colliders, density: f64::from(WATER_DENSITY) }
            .validate()?;
        let reset_edge = previous_reset.is_some_and(|previous| previous != observation.reset);
        *previous_reset = Some(observation.reset);
        if reset_edge || owner.as_ref().is_none_or(|owner| !owner.matches(&observation.inputs, *walls, *colliders)) {
            *epochs += 1;
            *owner = Some(LiquidRigidOwner::new(&observation.inputs, *walls, *colliders, *epochs, owner.as_ref())?);
            *owner_fresh = true;
        }
        Ok(true)
    }

    /// Exchange before tick `tick` of this frame (counted from its first):
    /// the GPU has finished tick `tick − 1`, so its reaction is final and the
    /// body rows are free to rewrite.
    fn exchange_tick(&mut self, tick: u32, exchange: Exchange) -> Result<(), String> {
        let owner = self.coupled.owner.as_mut().ok_or("GPU FLIP coupling: no coupled rigid world")?;
        let reaction = self.reaction.as_ref().ok_or("GPU FLIP coupling: the reaction is missing")?;
        // A tick running this frame started by now, so the tick before it
        // has its end sampled.
        let end = self.coupled.scenes.endpoint(owner.completed() + 1);
        owner.settle_ready(end, |_| true, |_, rows, impulses| {
            decode_reaction(exchange.offset, rows, reaction_floats(Some(reaction)), impulses)
        })?;
        let (offset, rows) = self.bodies.set_coupled_rows(tick as usize, owner.rows())?;
        let bodies = self.body_buffers.bodies().ok_or("GPU FLIP coupling: the body rows are missing")?;
        let bytes: &[u8] = bytemuck::cast_slice(rows);
        if offset + bytes.len() as u64 > bodies.size {
            return Err("GPU FLIP coupling: the body rows outgrew their buffer".into());
        }
        // SAFETY: shared storage in bounds (checked above); the GPU has
        // completed every command that touched it, and the next reader is
        // encoded after this write.
        unsafe { bodies.write(offset, bytes) };
        let (offset, contacts) = self.bodies.set_coupled_contacts(tick as usize, owner.contacts())?;
        let buffer = self.body_buffers.contacts().ok_or("GPU FLIP coupling: the contact normals are missing")?;
        let bytes: &[u8] = bytemuck::cast_slice(contacts);
        if offset + bytes.len() as u64 > buffer.size {
            return Err("GPU FLIP coupling: the contact normals outgrew their buffer".into());
        }
        // SAFETY: as the rows above.
        unsafe { buffer.write(offset, bytes) };
        // The CFL samples must belong to the same accepted tick as its body
        // rows. Reusing the first tick's world-space hull with the next tick's
        // centre changes the angular point speed and leaves eligibility stale.
        // This uses the existing coupled-tick fence; no additional wait.
        self.bodies.prepare_clock_vertices(self.coupled.walls.min, self.coupled.walls.size, tick as usize);
        for (port, vertices) in [
            ("clock_obstacles", self.bodies.clock_obstacles()),
            ("clock_sources", self.bodies.clock_sources()),
        ] {
            let buffer = self.body_buffers.output(port).ok_or_else(|| format!("GPU FLIP coupling: {port} is missing"))?;
            let bytes = bytemuck::cast_slice(vertices);
            if bytes.len() as u64 > buffer.size {
                return Err(format!("GPU FLIP coupling: {port} outgrew its buffer"));
            }
            // SAFETY: the same completed fence and bounds check as body rows.
            unsafe { buffer.write(0, bytes) };
        }
        reaction.zero_fill();
        owner.set_pending(PendingTick { tick: exchange.pending.tick + u64::from(tick),
            interval: exchange.frame.interval(u64::from(tick)).expect("accepted exchange interval"), ..exchange.pending });
        Ok(())
    }

    /// A restart the water's own clock found (its reset, a backward seek):
    /// the rigid world restarts with it, keeping its hulls.
    fn rebuild_owner(&mut self) -> Result<(), String> {
        let observation = self.coupled.observation.as_ref().ok_or("GPU FLIP coupling: the rigid observation is missing")?;
        self.coupled.epochs += 1;
        let owner = LiquidRigidOwner::new(&observation.inputs, self.coupled.walls, self.coupled.colliders, self.coupled.epochs, self.coupled.owner.as_ref())?;
        self.coupled.owner = Some(owner);
        Ok(())
    }
}

const _: () = assert!(ROLE_PORTS.len() == MAX_FLUID_ROLES);

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(resolution: f32, fill: f32, volume: Option<Transform>) -> Result<GpuFlipGeometry, String> {
        let read = |name: &str, default: f32| match name {
            "resolution" => resolution,
            "fill_height" => fill,
            _ => default,
        };
        gpu_flip_geometry(read, None, volume)
    }

    /// A later tick of a frame reads the obstacle bound its host step
    /// prepared, or none; it never inherits the first tick's.
    #[test]
    fn gpu_flip_domain_later_ticks_get_their_own_obstacle_bound() {
        use manifold_physics::clock::SimulationClock;
        let mut clock = SimulationClock::default();
        clock.advance(0.0, 0.5, 1.0, 0.0, false, true);
        let frame = clock.advance(1.0, 0.5, 1.0, 0.0, false, true);
        assert!(frame.ticks >= 2, "the fixture needs a later tick");
        let mut domain = GpuFlipDomain::new();
        domain.scheduled_frame = Some(frame);
        let scalars = |domain: &GpuFlipDomain, iteration| {
            domain.substep_clock_interval(iteration).expect("an accepted interval").scalars.to_vec()
        };
        assert!(scalars(&domain, 0).is_empty(), "the first tick keeps the frame's bound");
        assert_eq!(scalars(&domain, 1), vec![("initial_obstacle_speed", -1.0)]);
        domain.later_obstacle[0].1 = 2.5;
        domain.later_obstacle_tick = Some(1);
        assert_eq!(scalars(&domain, 1), vec![("initial_obstacle_speed", 2.5)]);
        domain.later_obstacle_tick = Some(2);
        assert_eq!(scalars(&domain, 1), vec![("initial_obstacle_speed", -1.0)], "another tick's bound");
    }

    #[test]
    fn gpu_flip_domain_fields_cover_the_native_solver_faces() {
        let geometry = geometry(64.0, 0.16, None).unwrap();
        let grid = FlipSolverGrid::from_lattice(geometry.setup.lattice);
        let field = geometry.field_lattice();
        let expected = FieldLattice::covering(grid.min(), geometry.setup.lattice.cell_size(), grid.nodes());
        assert_eq!(field, expected);
        assert!(geometry.outputs().iter().any(|(name, value)| *name == "mesh_wall_inset" && *value == grid.wall_inset()));
    }

    /// The frame publisher reads these CPU outputs before any solver publish.
    /// This guards the GPU FLIP side of BUG-a1xh independently of the CPU
    /// FLIP particle ring: neither surface nodes nor bin size need GPU data.
    #[test]
    fn first_frame_gpu_flip_lattice_is_valid_before_gpu_publish() {
        for resolution in [8.0, 32.0, 48.0, 64.0] {
            let setup = geometry(resolution, 0.16, None).unwrap();
            let outputs = setup.outputs();
            let lattice = LiquidLattice::from_scalars(|name, default| {
                outputs.iter().find(|(port, _)| *port == name).map_or(default, |(_, value)| *value)
            }).unwrap();
            assert_eq!(lattice, setup.setup.lattice);
            assert_eq!(lattice.nodes(), [resolution as u32 + 7; 3]);
            for (size, nodes) in lattice.bounds().scale.into_iter().zip(lattice.nodes()) {
                let cell = size / (nodes - 1) as f32;
                assert!(size.is_finite() && size > 0.0);
                assert!(cell.is_finite() && cell > 0.0);
                assert!((cell - lattice.cell_size()).abs() < 1e-6);
            }
        }
    }

    /// Any side the slider reaches is a lattice: the solver halves sides
    /// rounding up, so none needs to divide by a power of two.
    #[test]
    fn gpu_flip_domain_takes_any_resolution() {
        for resolution in [8.0, 24.0, 32.0, 63.0, 72.0, 100.0, 128.0] {
            let at = geometry(resolution, 0.16, None).unwrap_or_else(|reason| panic!("{resolution}: {reason}"));
            let n = resolution as u32;
            assert_eq!(at.setup.lattice.cells(), [n; 3]);
            assert_eq!(at.setup.lattice.nodes(), [n + 7; 3]);
        }
    }

    /// The Dam Break's column, as the preset wires it: the engine's boxes on
    /// the engine's half-cell site rule.
    #[test]
    fn gpu_flip_domain_fills_the_dam_break_sites() {
        let column = Transform { pos: [-1.25, 1.12, 0.0], scale: [1.18, 1.92, 3.5], ..Transform::default() };
        let at64 = geometry(64.0, 0.16, Some(column)).expect("64 fills");
        assert_eq!((at64.setup.pool_sites, at64.setup.box_sites), (5, [[5, 43], [5, 67], [8, 120]]));
        assert_eq!(at64.particles, 128 * 5 * 128 + 38 * 62 * 112);
        assert_eq!(at64.setup.lattice.nodes(), [71; 3]);
        let mass = at64.outputs().iter().find(|(name, _)| *name == "particle_mass").expect("mass").1;
        assert!((mass - 0.030_517_578).abs() < 1e-9, "{mass}");
    }

    #[test]
    fn gpu_flip_domain_refuses_by_name() {
        let refused = |result: Result<GpuFlipGeometry, String>| result.expect_err("refused");
        assert!(refused(geometry(64.0, 4.0, None)).contains("Initial Fill Height"));
        let turned = Transform { pos: [0.0, 1.0, 0.0], scale: [1.0; 3], rot_euler: [0.0, 0.3, 0.0], ..Transform::default() };
        assert!(refused(geometry(64.0, 0.16, Some(turned))).contains("initial volume"));
        // 256³ with a 2.5 m pool is 8 · 256² · 160 particles, past 2^24.
        let over = refused(geometry(256.0, 2.5, None));
        assert!(over.contains("Resolution") && over.contains("Initial Fill Height"), "{over}");
    }

    /// Max Iterations defaults to the engine's 900, rides the geometry to the
    /// `max_iterations` output, and is refused outside 1..=900.
    #[test]
    fn gpu_flip_domain_carries_max_iterations() {
        let at = |value: Option<f32>| {
            let read = |name: &str, default: f32| match (name, value) {
                ("fill_height", _) => 0.16,
                ("max_iterations", Some(v)) => v,
                _ => default,
            };
            gpu_flip_geometry(read, None, None)
        };
        assert_eq!(at(None).expect("default").max_iterations, 900);
        assert_eq!(at(Some(120.0)).expect("120").max_iterations, 120);
        let refused = at(Some(0.0)).expect_err("0");
        assert!(refused.contains("Max Iterations must be 1 to 900"), "{refused}");
        assert!(at(Some(f32::NAN)).is_err());
        assert!(OUTPUTS.contains(&"max_iterations"));
    }

    #[test]
    fn gpu_flip_domain_carries_sheet_fill_rate_without_restarting() {
        let at = |value: Option<f32>| gpu_flip_geometry(|name, default| match name {
            "fill_height" => 0.16,
            "sheet_fill_rate" => value.unwrap_or(default),
            _ => default,
        }, None, None);
        let off = at(None).expect("default");
        assert_eq!(off.sheet_fill_rate, 0.0);
        for rate in [0.0, 0.25, 0.75, 1.0] {
            let live = at(Some(rate)).expect("valid rate");
            assert_eq!(live.sheet_fill_rate, rate);
            assert_eq!(live.setup, off.setup, "rate must not reset the liquid");
        }
        for bad in [-0.01, 1.01, f32::NAN, f32::INFINITY] {
            assert!(at(Some(bad)).unwrap_err().contains("Sheet Fill Rate"));
        }
        assert!(OUTPUTS.contains(&"sheet_fill_rate"));
    }

    /// Solve Level is refused past the lattice's levels, never clamped: 64
    /// has native levels 67, 34, 17, 9, 5, 3, so 4 is the deepest gradient level.
    #[test]
    fn gpu_flip_domain_refuses_a_solve_level_the_lattice_lacks() {
        let at = |resolution: f32, level: f32| {
            let read = |name: &str, default: f32| match name {
                "resolution" => resolution,
                "fill_height" => 0.16,
                "solve_level" => level,
                _ => default,
            };
            gpu_flip_geometry(read, None, None)
        };
        assert_eq!(at(64.0, 3.0).expect("level 3 at 64").solve_level, 3);
        assert_eq!(at(64.0, 0.0).expect("level 0").solve_level, 0);
        assert_eq!(at(64.0, 4.0).expect("native level 4").solve_level, 4);
        let refused = at(64.0, 5.0).expect_err("level 5 at 64");
        assert!(refused.contains("Solve Level must be 0 to 4"), "{refused}");
        let fraction = at(64.0, 1.5).expect_err("a fraction");
        assert!(fraction.contains("Solve Level must be a whole number"), "{fraction}");
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(any(test, feature = "testkit"))]
impl GpuFlipSetup {
    pub fn box_sites_for_test(&self) -> [[u32; 2]; 3] { self.box_sites }
    pub fn pool_sites_for_test(&self) -> u32 { self.pool_sites }
}

#[cfg(any(test, feature = "testkit"))]
impl GpuFlipSetup {
    pub fn lattice_for_test(&self) -> LiquidLattice { self.lattice }
}

#[cfg(any(test, feature = "testkit"))]
impl GpuFlipGeometry {
    pub fn setup_for_test(&self) -> GpuFlipSetup { self.setup }
    pub fn sheet_fill_rate_for_test(&self) -> f32 { self.sheet_fill_rate }
}
