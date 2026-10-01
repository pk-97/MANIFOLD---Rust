//! `node.matter_domain` — the scene-facing CPU bridge of a matter domain
//! (`docs/GPU_MPM_SOLVER_DESIGN.md` D17): it speaks `node.fluid_surface`'s
//! scene contract (names, types, meanings) and turns it into the lattice,
//! fill boxes, the fixed-tick clock (D8) and the per-tick substep count and
//! material dials (D3, D4) the matter atoms read as wires. P1 carries the
//! domain, walls, box fill, clock, gravity, speed, reset, seed and the water
//! dials; P2a Collider roles as bodies, shapes and a distance atlas (D11);
//! P2b the scene's Box3D bodies as two-way coupled bodies (section 5): the
//! domain owns the rigid world in-thread and steps it one settled fluid tick
//! at a time. Scene forces and impulses reach the liquid through the shared
//! field lattices of `liquid::fields` (seam P8).
//! Exempt from the codegen mandate as a CPU bridge (ADDING_PRIMITIVES.md
//! exclusion 3).

use std::borrow::Cow;

use manifold_gpu::{FrameClock, GpuBinding, GpuBuffer, GpuComputePipeline};
use manifold_physics::FieldValue;

use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid::{CoupledRigidFrame, CoupledRigidInputs, FluidDomainLayout, TICK, domain_layout};
use crate::node_graph::fluid_role::{FluidRole, MAX_FLUID_ROLES};
use crate::node_graph::liquid::bodies::{BodiesStatus, LiquidBodies, LiquidBody, LiquidShape};
use crate::node_graph::liquid::clock::LiquidClock;
use crate::node_graph::liquid::coupling::{LiquidRigidOwner, PendingTick, takes_reaction};
use crate::node_graph::liquid::fields::{self, FieldLattice, LiquidFields, LiquidImpulses};
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::matter::coupling::{ReactionScale, body_limit, decode};
use crate::node_graph::matter::{
    MAX_SUBSTEPS, REACTION_WORDS, WATER_DENSITY, block_sort_box, free_fall_speed, lattice_blocks, lattice_nodes,
    momentum_unit, stiffness_fitting_cap, substeps_per_tick, water_lambda, wave_speed,
};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::physics::{
    RigidImpulseTargets, RigidSceneObservation, offline_simulation,
};
use crate::node_graph::physics_events::ResolvedNodeImpulse;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::transform::Transform;

/// Everything whose change restarts the simulation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatterSetup {
    pub(crate) lattice: LiquidLattice,
    pub(crate) closed_faces: u32,
    pub(crate) pool_cells: u32,
    pub(crate) column: [[u32; 2]; 3],
    pub(crate) points_per_cell: u32,
    pub(crate) seed: u32,
}

/// The domain's setup and the layout it came from, computed from params and
/// wires alone: the node and the extent checker both call
/// [`matter_geometry`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MatterGeometry {
    pub(crate) layout: FluidDomainLayout,
    pub(crate) setup: MatterSetup,
}

impl MatterGeometry {
    /// The scalar outputs fixed by the setup, by name.
    pub(crate) fn outputs(&self) -> [(&'static str, f32); 27] {
        let MatterSetup { lattice, closed_faces, pool_cells, column, points_per_cell, seed } = self.setup;
        let blocks = lattice_blocks(&lattice);
        let (centre, size, bin) = block_sort_box(&lattice);
        [
            ("lattice_min_x", lattice.min()[0]),
            ("lattice_min_y", lattice.min()[1]),
            ("lattice_min_z", lattice.min()[2]),
            ("cell_size", lattice.cell_size()),
            ("nodes_x", lattice.nodes()[0] as f32),
            ("nodes_y", lattice.nodes()[1] as f32),
            ("nodes_z", lattice.nodes()[2] as f32),
            ("closed_faces", closed_faces as f32),
            ("pool_cells", pool_cells as f32),
            ("column_x0", column[0][0] as f32),
            ("column_x1", column[0][1] as f32),
            ("column_y0", column[1][0] as f32),
            ("column_y1", column[1][1] as f32),
            ("column_z0", column[2][0] as f32),
            ("column_z1", column[2][1] as f32),
            ("points_per_cell", points_per_cell as f32),
            ("fill_seed", seed as f32),
            ("blocks_x", blocks[0] as f32),
            ("blocks_y", blocks[1] as f32),
            ("blocks_z", blocks[2] as f32),
            ("block_center_x", centre[0]),
            ("block_center_y", centre[1]),
            ("block_center_z", centre[2]),
            ("block_size_x", size[0]),
            ("block_size_y", size[1]),
            ("block_size_z", size[2]),
            ("block_cell_size", bin),
        ]
    }
}

/// The fill in the layout's cells: the pool's height in cells and the
/// initial volume's cell range per axis (empty without one), refused by name
/// when either does not fit the domain.
pub(crate) fn fill_region(
    layout: &FluidDomainLayout,
    fill_height: f32,
    initial_volume: Option<Transform>,
) -> Result<(u32, [[u32; 2]; 3]), String> {
    // The lattice's f32 cell size, as every matter atom reads it.
    let dx = f64::from(layout.cell_size as f32);
    if !fill_height.is_finite() || fill_height < 0.0 || fill_height >= layout.size[1] {
        return Err("Matter: Initial Fill Height must lie within the domain height".into());
    }
    let pool_cells = ((f64::from(fill_height) / dx).round() as u32).min(layout.cells[1]);
    let mut column = [[0u32; 2]; 3];
    if let Some(volume) = initial_volume {
        if volume.billboard || volume.rot_euler.iter().any(|v| !v.is_finite() || v.abs() > 1e-6) {
            return Err("Matter: the initial volume must be an axis-aligned box; rotation and billboarding are not supported".into());
        }
        if volume.pos.iter().chain(&volume.scale).any(|v| !v.is_finite())
            || volume.scale.iter().any(|v| *v <= 0.0)
            || (0..3).any(|d| {
                let half = volume.scale[d] * 0.5;
                volume.pos[d] - half < layout.min[d] - 1e-4
                    || volume.pos[d] + half > layout.min[d] + layout.size[d] + 1e-4
            })
        {
            return Err("Matter: the initial volume must be a finite box fully inside the domain".into());
        }
        for (d, range) in column.iter_mut().enumerate() {
            let cell = |x: f32| (((f64::from(x) - f64::from(layout.min[d])) / dx).round().max(0.0) as u32).min(layout.cells[d]);
            *range = [
                cell(volume.pos[d] - volume.scale[d] * 0.5),
                cell(volume.pos[d] + volume.scale[d] * 0.5),
            ];
        }
    }
    Ok((pool_cells, column))
}

/// The domain box, lattice, walls and fill from the domain's params and
/// wires (`read` is `scalar_or_param`), refused by name when the lattice is
/// over Grid Budget or the fill does not fit the domain.
pub(crate) fn matter_geometry(
    read: impl Fn(&str, f32) -> f32,
    params: &ParamValues,
    domain: Option<Transform>,
    initial_volume: Option<Transform>,
) -> Result<MatterGeometry, String> {
    let resolution = read("resolution", 64.0).round().max(0.0) as u32;
    let layout = domain_layout(domain, read("domain_size", 4.0), resolution)?;
    let lattice = LiquidLattice::from_layout(&layout);
    let budget = match params.get("grid_budget_mcells") {
        Some(ParamValue::Float(budget)) => *budget,
        _ => 8.0,
    };
    admit_lattice(&lattice, budget)?;
    let (pool_cells, column) = fill_region(&layout, read("fill_height", 0.4), initial_volume)?;
    let points_per_cell = match params.get("points_per_cell") {
        Some(ParamValue::Enum(1)) => 27,
        _ => 8,
    };
    let seed = match params.get("seed") {
        Some(ParamValue::Float(seed)) => *seed,
        _ => 0.0,
    };
    let setup = MatterSetup {
        lattice,
        closed_faces: closed_faces(params),
        pool_cells,
        column,
        points_per_cell,
        seed: seed.round().clamp(0.0, 16_777_215.0) as u32,
    };
    Ok(MatterGeometry { layout, setup })
}

const UPLOAD_SHADER: &str = include_str!("shaders/matter_domain_upload.wgsl");
/// 16-byte groups one inline upload carries (setBytes stays under 4 KB).
const UPLOAD_GROUPS: usize = 254;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UploadParams {
    start: u32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
    words: [[u32; 4]; 254],
}

const _: () = assert!(std::mem::size_of::<UploadParams>() < 4096);

/// The provided body, shape and atlas storage. Shapes and the atlas are
/// rebuilt into fresh buffers (never in flight); body rows are uploaded in
/// encoder order each frame.
pub struct BodyBuffers {
    bodies: GpuBuffer,
    shapes: GpuBuffer,
    atlas: GpuBuffer,
    version: u64,
}

crate::primitive! {
    name: MatterDomain,
    type_id: "node.matter_domain",
    purpose: "Define a live GPU liquid domain with node.fluid_surface's scene contract: the axis-aligned domain box, resolution, six closed faces, initial fill height and box, gravity, simulation speed, reset and seed, plus the matter dials Points per Cell, Stiffness, Cohesion and Liveliness, and up to 64 Collider roles. Outputs the lattice, fill boxes, this frame's fixed 60 Hz ticks and substeps per tick, the epoch and the display clock for the Live Matter atoms, and the colliders as one body row per collider per tick of this frame (its pose at the tick's start and its motion over the tick), a shape per collider and their distance lattices packed in one half-precision atlas. Paired with a physics world in the same scene, it owns that world and couples its bodies both ways: they join the body rows, the liquid's push on them comes back through the reaction array, and the world steps once per fluid tick after the GPU has finished it.",
    inputs: {
        domain: Transform optional,
        initial_volume: Transform optional,
        resolution: ScalarF32 optional,
        domain_size: ScalarF32 optional,
        fill_height: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        speed: ScalarF32 optional,
        reset: ScalarF32 optional,
        stiffness: ScalarF32 optional,
        cohesion: ScalarF32 optional,
        liveliness: ScalarF32 optional,
        acceleration_field: VectorField optional,
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
        cell_size: ScalarF32,
        nodes_x: ScalarF32, nodes_y: ScalarF32, nodes_z: ScalarF32,
        closed_faces: ScalarF32,
        gravity_x: ScalarF32, gravity: ScalarF32, gravity_z: ScalarF32,
        pool_cells: ScalarF32,
        column_x0: ScalarF32, column_x1: ScalarF32,
        column_y0: ScalarF32, column_y1: ScalarF32,
        column_z0: ScalarF32, column_z1: ScalarF32,
        points_per_cell: ScalarF32,
        fill_seed: ScalarF32,
        ticks: ScalarF32,
        substeps_per_tick: ScalarF32,
        epoch: ScalarF32,
        simulation_time: ScalarF32,
        display_time: ScalarF32,
        dropped_seconds: ScalarF32,
        lambda: ScalarF32,
        cohesion: ScalarF32,
        liveliness: ScalarF32,
        density: ScalarF32,
        limited_by_substeps: ScalarF32,
        blocks_x: ScalarF32, blocks_y: ScalarF32, blocks_z: ScalarF32,
        block_center_x: ScalarF32, block_center_y: ScalarF32, block_center_z: ScalarF32,
        block_size_x: ScalarF32, block_size_y: ScalarF32, block_size_z: ScalarF32,
        block_cell_size: ScalarF32,
        momentum_unit: ScalarF32,
        body_count: ScalarF32, body_rows: ScalarF32, first_tick: ScalarF32,
        dynamic_count: ScalarF32,
        field_nodes_x: ScalarF32, field_nodes_y: ScalarF32, field_nodes_z: ScalarF32,
        field_spacing: ScalarF32,
        force_lattices: ScalarF32,
        impulse_tick: ScalarF32,
        bodies: Array(LiquidBody), shapes: Array(LiquidShape), atlas: Array(u32),
        reaction: Array(i32),
        forces: Array(f32), impulses: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("seed"), label: "Seed", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16777215.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("resolution"), label: "Resolution", ty: ParamType::Int, default: ParamValue::Float(64.0), range: Some((8.0, 512.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("grid_budget_mcells"), label: "Grid Budget (M cells)", ty: ParamType::Float, default: ParamValue::Float(8.0), range: Some((0.01, 512.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("domain_size"), label: "Domain Size", ty: ParamType::Float, default: ParamValue::Float(4.0), range: Some((0.5, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_x"), label: "Closed −X", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_x"), label: "Closed +X", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_y"), label: "Closed Bottom", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_y"), label: "Closed Top", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_z"), label: "Closed −Z", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_z"), label: "Closed +Z", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_height"), label: "Initial Fill Height", ty: ParamType::Float, default: ParamValue::Float(0.4), range: Some((0.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("speed"), label: "Simulation Speed", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("reset"), label: "Reset", ty: ParamType::Trigger, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("points_per_cell"), label: "Points per Cell", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 1.0)), enum_values: &["8", "27"] },
        ParamDef { name: Cow::Borrowed("stiffness"), label: "Stiffness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.5, 3.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cohesion"), label: "Cohesion", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("liveliness"), label: "Liveliness", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "The Live Matter group's source of truth: wire its lattice, fill, clock and dial outputs into node.matter_fill, node.matter_state, the region body atoms and node.matter_frame. The domain box, resolution, faces, fill, Points per Cell and Seed restart the simulation; gravity, Simulation Speed, Stiffness, Cohesion and Liveliness are live. Stiffness sets how springy the water is and costs substeps (Stiffness 0.5 → 21, 1 → 34, 2 → 61 at 64³ in 4 m); a value that would need more than 128 runs at the largest that fits and reports it on limited_by_substeps. Live runs at most three ticks per display frame and reports dropped time; export runs every tick. Collider roles (node.fluid_role_source, Role Collider) move live and restart nothing; bodies, first_tick, body_count and body_rows feed node.matter_move_bodies, and shapes and atlas node.matter_grid_update and node.liquid_solid_distance. Until every collider's distance lattice is built the liquid holds. Fill, Inflow and Outflow roles are refused until sources and drains arrive. In a scene with a node.physics_world, the world's bodies selected as colliders couple both ways: wire reaction into node.matter_move_bodies and node.matter_body_reaction (whose reaction_out feeds node.grid_to_matter), and dynamic_count into all three. Live, a coupled domain runs at most one tick per display frame and holds while the GPU is still finishing the last one; export runs every tick, waiting for the GPU between ticks so Box3D and the liquid exchange once per tick at any frame rate; light bodies raise the substep count, and one too light for 128 substeps is refused by name. Scene forces arrive on acceleration_field and scene impulses through the impulse hooks; both are sampled on a coarse field lattice (a quarter of the resolution per axis): wire forces, impulses and the six field scalars into node.matter_grid_update and node.matter_body_reaction. Forces act every substep; an impulse changes the water's velocity once, on the first substep of impulse_tick, the first tick after the frame it was fired. A hit fired while paused or at Simulation Speed 0 is discarded, never replayed on resume; a restart cancels hits not yet applied. Rigid targets of an impulse reach the coupled bodies.",
    examples: ["WaterDamBreakMatter", "WaterStillPoolMatter", "WaterFloatingBoxMatter"],
    picker: { label: "Matter Domain", category: Atom },
    summary: "Sets up a live GPU liquid: its box, resolution, walls, starting fill, gravity and how the water behaves.",
    category: Particles3D,
    role: Source,
    aliases: ["matter", "mpm", "live water", "gpu liquid", "liquid domain"],
    boundary_reason: NonGpu,
    extra_fields: {
        clock: LiquidClock = LiquidClock::default(),
        setup: Option<MatterSetup> = None,
        limited: bool = false,
        published: Option<[f32; OUTPUTS.len()]> = None,
        bodies: LiquidBodies = LiquidBodies::default(),
        role_pending: bool = false,
        body_buffers: Option<BodyBuffers> = None,
        upload: Option<GpuComputePipeline> = None,
        body_rows: f32 = 0.0,
        rows_fresh: bool = false,
        coupled: Coupling = Coupling::default(),
        reaction: Option<GpuBuffer> = None,
        impulses: LiquidImpulses = LiquidImpulses::default(),
        fields: LiquidFields = LiquidFields::default(),
        acceleration: Option<FieldValue> = None,
    },
}

/// The coupled half of the domain (section 5): the paired physics world's
/// latest observation, and the rigid owner built from it.
#[derive(Default)]
pub struct Coupling {
    mode: bool,
    observation: Option<RigidSceneObservation>,
    colliders: RigidImpulseTargets,
    error: Option<String>,
    previous_reset: Option<f32>,
    owner: Option<LiquidRigidOwner>,
    /// How the owner's pending tick's reaction words decode.
    scale: Option<ReactionScale>,
    /// The owner was built since the clock last restarted: the next frame
    /// restarts the liquid with it.
    owner_fresh: bool,
    epochs: u64,
    /// This frame's compute failed; nothing is published to the pair.
    failed: bool,
    /// Transport of the last frame the pair advanced at.
    transport: Option<f64>,
    /// This offline frame runs several coupled ticks, exchanging with Box3D
    /// between them.
    exchange: Option<Exchange>,
    /// A host step failed; the next frame reports it and restarts the pair.
    host_error: Option<String>,
}

/// An offline frame of several coupled ticks: the region syncs at each later
/// tick's first substep, and the domain settles the tick before it there.
#[derive(Clone, Copy)]
struct Exchange {
    substeps: u32,
    ticks: u32,
    /// The frame's first tick; later ticks differ only in `tick`.
    pending: PendingTick,
    scale: ReactionScale,
}

/// The reaction words, once their last GPU writer has retired.
fn reaction_words(buffer: Option<&GpuBuffer>) -> Option<&[i32]> {
    let buffer = buffer?;
    let words = buffer.mapped_ptr()?.cast::<i32>().cast_const();
    // SAFETY: shared, 4-byte aligned storage whose last GPU writer has
    // retired (the caller checked the frame clock or waited on the GPU).
    Some(unsafe { std::slice::from_raw_parts(words, (buffer.size / 4) as usize) })
}

/// The Grid Budget gate: a lattice over `budget_mcells` million nodes is
/// refused by name before anything downstream sizes or dispatches over it.
pub(crate) fn admit_lattice(lattice: &LiquidLattice, budget_mcells: f32) -> Result<(), String> {
    let nodes = lattice_nodes(lattice.nodes()) as f64;
    if !budget_mcells.is_finite() || budget_mcells <= 0.0 || nodes > f64::from(budget_mcells) * 1e6 {
        return Err(format!(
            "Matter lattice needs {:.3} million nodes; Grid Budget is {budget_mcells:.3} million. Increase Grid Budget or lower Resolution. GPU time grows with node count.",
            nodes / 1e6
        ));
    }
    Ok(())
}

impl Coupling {
    fn reset(&mut self) {
        let (mode, epochs) = (self.mode, self.epochs);
        *self = Self { mode, epochs, ..Self::default() };
    }
}

fn closed_faces(params: &ParamValues) -> u32 {
    ["closed_neg_x", "closed_pos_x", "closed_neg_y", "closed_pos_y", "closed_neg_z", "closed_pos_z"]
        .iter()
        .enumerate()
        .fold(0, |mask, (bit, name)| {
            let closed = !matches!(params.get(*name), Some(ParamValue::Bool(false)));
            mask | (u32::from(closed) << bit)
        })
}

/// Every scalar output, in the order [`MatterDomain::compute`] fills them.
const OUTPUTS: [&str; 52] = [
    "lattice_min_x", "lattice_min_y", "lattice_min_z", "cell_size", "nodes_x", "nodes_y",
    "nodes_z", "closed_faces", "gravity_x", "gravity", "gravity_z", "pool_cells", "column_x0",
    "column_x1", "column_y0", "column_y1", "column_z0", "column_z1", "points_per_cell",
    "fill_seed", "ticks", "substeps_per_tick", "epoch", "simulation_time", "display_time",
    "dropped_seconds", "lambda", "cohesion", "liveliness", "density", "limited_by_substeps",
    "blocks_x", "blocks_y", "blocks_z", "block_center_x", "block_center_y", "block_center_z",
    "block_size_x", "block_size_y", "block_size_z", "block_cell_size", "momentum_unit",
    "body_count", "body_rows", "first_tick", "dynamic_count", "field_nodes_x", "field_nodes_y",
    "field_nodes_z", "field_spacing", "force_lattices", "impulse_tick",
];
/// Wired role ports, in slot order (node.fluid_surface's names).
const ROLE_PORTS: [&str; MAX_FLUID_ROLES] = [
    "role_0", "role_1", "role_2", "role_3", "role_4", "role_5", "role_6", "role_7", "role_8",
    "role_9", "role_10", "role_11", "role_12", "role_13", "role_14", "role_15", "role_16", "role_17",
    "role_18", "role_19", "role_20", "role_21", "role_22", "role_23", "role_24", "role_25", "role_26",
    "role_27", "role_28", "role_29", "role_30", "role_31", "role_32", "role_33", "role_34", "role_35",
    "role_36", "role_37", "role_38", "role_39", "role_40", "role_41", "role_42", "role_43", "role_44",
    "role_45", "role_46", "role_47", "role_48", "role_49", "role_50", "role_51", "role_52", "role_53",
    "role_54", "role_55", "role_56", "role_57", "role_58", "role_59", "role_60", "role_61", "role_62",
    "role_63",
];
const TICKS: usize = 20;
const IMPULSE_TICK: usize = 51;
const _: () = assert!(matches!(OUTPUTS[TICKS].as_bytes(), b"ticks"));
const _: () = assert!(matches!(OUTPUTS[IMPULSE_TICK].as_bytes(), b"impulse_tick"));

impl Primitive for MatterDomain {
    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "bodies" | "shapes" | "atlas" | "reaction" | "forces" | "impulses")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "reaction" => return self.reaction.as_ref(),
            "forces" => return self.fields.forces_buffer(),
            "impulses" => return self.fields.impulses_buffer(),
            _ => {}
        }
        let buffers = self.body_buffers.as_ref()?;
        match port {
            "bodies" => Some(&buffers.bodies),
            "shapes" => Some(&buffers.shapes),
            "atlas" => Some(&buffers.atlas),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, grown at run time.
        matches!(port_name, "bodies" | "shapes" | "atlas" | "reaction" | "forces" | "impulses").then_some(1)
    }

    fn warmup_pending(&self) -> bool {
        self.role_pending
    }

    /// Offline coupled frames sync at each later tick's first substep.
    fn substep_host_sync(&self, iteration: u32) -> bool {
        self.coupled
            .exchange
            .is_some_and(|x| iteration.is_multiple_of(x.substeps) && iteration / x.substeps < x.ticks)
    }

    /// Section 5 between two ticks of one offline frame: settle the tick the
    /// GPU just finished, step Box3D over it, and hand the next tick the
    /// bodies' new state and a cleared reaction.
    fn substep_host_step(
        &mut self,
        iteration: u32,
        _gpu: Option<&mut crate::gpu_encoder::GpuEncoder<'_>>,
    ) -> Result<(), String> {
        let Some(exchange) = self.coupled.exchange else { return Ok(()) };
        let result = self.exchange_tick(iteration / exchange.substeps, exchange);
        if let Err(error) = &result {
            // The pair restarts next frame with a fresh rigid owner.
            self.coupled.exchange = None;
            self.coupled.owner = None;
            self.coupled.host_error = Some(error.clone());
        }
        result
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // A physics sample reads authored inputs only; it never advances time.
        if crate::node_graph::physics::authored_sample_only() {
            return;
        }
        let mut roles: [Option<FluidRole>; MAX_FLUID_ROLES] = std::array::from_fn(|_| None);
        self.role_pending = false;
        for (slot, port) in ROLE_PORTS.iter().enumerate() {
            if let Some(input) = ctx.inputs.slot(port) {
                roles[slot] = ctx.inputs.fluid_role(port);
                self.role_pending |= !ctx.inputs.slot_content_ready(input) || roles[slot].is_none();
            }
        }
        self.acceleration = ctx.inputs.vector_field("acceleration_field");
        if let Some(slot) = ctx.inputs.slot("acceleration_field") {
            self.role_pending |= !ctx.inputs.slot_content_ready(slot) || self.acceleration.is_none();
        }
        // Every output is published every frame, so no consumer reads a slot
        // this node left unwritten. While a role is still being prepared, or
        // on an error, the liquid holds: the last good outputs repeat with
        // zero ticks.
        self.coupled.exchange = None;
        let computed = if self.role_pending { Ok(None) } else { self.compute(ctx, &roles) };
        let held = || {
            let mut held = self.published.unwrap_or([0.0; OUTPUTS.len()]);
            held[TICKS] = 0.0;
            held[IMPULSE_TICK] = -1.0;
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
        if fresh {
            self.published = Some(values);
        }
        for (name, value) in OUTPUTS.iter().zip(values) {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
        self.upload_bodies(ctx, fresh && self.rows_fresh, values[TICKS] > 0.0);
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

    /// A coupled pair restarts together with a fresh rigid owner; the
    /// liquid's own clock handles seeks.
    fn clear_state(&mut self) {
        self.coupled.reset();
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

    fn physics_impulse_epoch(&self) -> Option<u64> {
        self.impulses.epoch()
    }

    /// A hit fired this frame is stamped at the liquid's simulated time, so
    /// it lands on the first tick of the next frame (the rigid half at the
    /// start of the next settled rigid tick).
    fn physics_impulse_stamp(
        &self,
        transport: manifold_core::Seconds,
        sequence: u64,
    ) -> Result<manifold_physics::input::EventStamp, String> {
        if self.role_pending || self.coupled.failed {
            return Err("Matter impulses: cannot capture an impulse while the liquid is pending or failed".into());
        }
        self.impulses.stamp(transport.0, sequence)
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

impl MatterDomain {
    /// Keep the provided body, shape and atlas buffers current: rebuilt
    /// shapes and atlas go into fresh buffers, this frame's body rows are
    /// written in encoder order.
    fn upload_bodies(&mut self, ctx: &mut EffectNodeContext<'_, '_>, rows_fresh: bool, ticks: bool) {
        let Some(gpu) = ctx.gpu.as_deref_mut() else { return };
        // Sized for every body a liquid holds, so a pending reaction never
        // moves when roles come and go.
        let reaction = self.reaction.get_or_insert_with(|| {
            let bytes = MAX_FLUID_ROLES * REACTION_WORDS as usize * 4;
            let buffer = gpu.device.create_buffer_shared(bytes as u64);
            // SAFETY: new shared buffer, not yet visible to the GPU.
            unsafe { buffer.write(0, &vec![0u8; bytes]) };
            buffer
        });
        // A coupled tick sums its reaction from zero; the words stay put
        // until the next tick so the owner reads them once the frame retires.
        if ticks && self.coupled.owner.is_some() {
            gpu.native_enc.clear_buffer(reaction);
        }
        let row_bytes = std::mem::size_of_val(self.bodies.last_rows());
        let needs_bodies = self
            .body_buffers
            .as_ref()
            .is_none_or(|buffers| buffers.bodies.size < row_bytes as u64);
        let version = self.bodies.version;
        if self.body_buffers.as_ref().is_none_or(|buffers| buffers.version != version) || needs_bodies {
            let fresh = |bytes: &[u8], least: usize| {
                let buffer = gpu.device.create_buffer_shared(bytes.len().max(least) as u64);
                // SAFETY: new shared buffer, not yet visible to the GPU.
                unsafe { buffer.write(0, bytes) };
                buffer
            };
            let bodies = match self.body_buffers.take() {
                Some(buffers) if !needs_bodies => buffers.bodies,
                _ => gpu.device.create_buffer_shared(row_bytes.max(std::mem::size_of::<LiquidBody>()) as u64),
            };
            self.body_buffers = Some(BodyBuffers {
                bodies,
                shapes: fresh(bytemuck::cast_slice(self.bodies.shapes()), std::mem::size_of::<LiquidShape>()),
                atlas: fresh(bytemuck::cast_slice(self.bodies.atlas()), 4),
                version,
            });
        }
        if !rows_fresh || row_bytes == 0 {
            return;
        }
        let pipeline = self
            .upload
            .get_or_insert_with(|| gpu.device.create_compute_pipeline(UPLOAD_SHADER, "cs_main", "node.matter_domain.bodies"));
        let target = &self.body_buffers.as_ref().expect("allocated above").bodies;
        let groups: &[[u32; 4]] = bytemuck::cast_slice(self.bodies.last_rows());
        for (chunk_index, chunk) in groups.chunks(UPLOAD_GROUPS).enumerate() {
            let mut params: UploadParams = bytemuck::Zeroable::zeroed();
            let UploadParams { start, count, words, .. } = &mut params;
            *start = (chunk_index * UPLOAD_GROUPS) as u32;
            *count = chunk.len() as u32;
            words[..chunk.len()].copy_from_slice(chunk);
            gpu.native_enc.dispatch_compute(
                pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                    GpuBinding::Buffer { binding: 1, buffer: target, offset: 0 },
                ],
                [(chunk.len() as u32).div_ceil(64), 1, 1],
                "node.matter_domain.bodies",
            );
        }
    }

    /// This frame's outputs, or None while a collider's distance lattice is
    /// still building (the liquid holds and the clock does not advance).
    fn compute(
        &mut self,
        ctx: &EffectNodeContext<'_, '_>,
        roles: &[Option<FluidRole>],
    ) -> Result<Option<[f32; OUTPUTS.len()]>, String> {
        let geometry = matter_geometry(
            |name, default| ctx.scalar_or_param(name, default),
            ctx.params,
            ctx.inputs.transform("domain"),
            ctx.inputs.transform("initial_volume"),
        )?;
        let MatterGeometry { layout, setup } = geometry;
        let lattice = setup.lattice;
        let dx = f64::from(lattice.cell_size());
        let speed = ctx.scalar_or_param("speed", 1.0);
        let live = ["gravity_x", "gravity", "gravity_z", "stiffness", "cohesion", "liveliness"]
            .map(|name| ctx.scalar_or_param(name, 0.0));
        if !speed.is_finite() || !(0.0..=4.0).contains(&speed) || live.iter().any(|v| !v.is_finite()) {
            return Err("Matter: Simulation Speed must be between 0 and 4, and gravity and the dials must be finite".into());
        }
        let offline = offline_simulation();
        if let Some(error) = self.coupled.host_error.take() {
            return Err(error);
        }
        let clock = if self.coupled.mode { ctx.gpu.as_deref().and_then(|gpu| gpu.device.frame_clock()) } else { None };
        if self.coupled.mode && !self.observe_rigid(ctx.time.seconds.0, speed)? {
            return Ok(None);
        }
        let coupled_geometries = self.coupled.owner.as_ref().map_or(&[][..], LiquidRigidOwner::geometries);
        if self.bodies.prepare(roles, coupled_geometries, lattice.cell_size(), offline)? == BodiesStatus::Pending {
            return Ok(None);
        }
        let setup_changed = self.setup != Some(setup);
        let restart = setup_changed || self.coupled.owner_fresh;
        // Section 5: live, the liquid runs at most the one tick whose bodies
        // Box3D has settled, and none while the last tick's reaction is in
        // flight. Offline waits for it, then runs every due tick, exchanging
        // with Box3D between them through the region's host syncs.
        let cap = match (&mut self.coupled.owner, &self.coupled.observation) {
            (Some(owner), Some(observation)) if !restart => {
                let reaction = self.reaction.as_ref();
                let scale = self.coupled.scale;
                // This frame's clear is encoded after this read.
                let ticks = owner.settle(
                    &observation.inputs,
                    |stamp| clock.as_ref().is_none_or(|clock| if offline { clock.wait(stamp) } else { clock.is_complete(stamp) }),
                    |_, rows, impulses| {
                        let scale = scale.ok_or("Matter coupling: the pending tick has no reaction scale")?;
                        decode(scale, rows, reaction_words(reaction), impulses)
                    },
                )?;
                if offline && ticks > 0 { None } else { Some(ticks) }
            }
            (Some(_), _) => Some(1),
            (None, _) => None,
        };
        self.clock.set_tick_cap(cap);
        self.setup = Some(setup);
        let frame = self.clock.advance(
            ctx.time.seconds.0,
            ctx.time.delta.0,
            speed,
            ctx.scalar_or_param("reset", 0.0),
            restart,
            offline,
        );
        if frame.restarted && self.coupled.owner.is_some() && !self.coupled.owner_fresh {
            self.rebuild_owner()?;
        }
        self.coupled.owner_fresh = false;
        self.impulses.observe_frame(ctx.time.seconds.0, &frame)?;
        let consumed = frame.simulation_time - f64::from(frame.ticks) * TICK;
        self.bodies.observe(roles, frame.epoch, frame.target_time, consumed)?;
        let first_tick = fields::first_tick(&frame);
        // A restart runs no tick yet still publishes the first tick's rows: the
        // fill seeds around the colliders' starting poses. Otherwise a frame
        // without ticks keeps the last rows as the bodies' poses.
        let row_ticks = if frame.restarted { frame.ticks.max(1) } else { frame.ticks };
        self.rows_fresh = row_ticks > 0;
        let coupled_rows = self.coupled.owner.as_ref().map_or(&[][..], LiquidRigidOwner::rows);
        let rows = if row_ticks > 0 {
            let rows = self.bodies.rows(first_tick, row_ticks, coupled_rows).len() as f32;
            self.body_rows = rows;
            rows
        } else {
            self.body_rows
        };
        let dynamic_count = coupled_rows.iter().filter(|row| takes_reaction(row)).count();

        // D4: substeps from the stiffness/CFL rule, from parameters only.
        let longest = f64::from(layout.size.iter().copied().fold(0.0f32, f32::max));
        let unit_wave = wave_speed(water_lambda(longest, 1.0), f64::from(WATER_DENSITY));
        let v_est = free_fall_speed(f64::from(layout.size[1]));
        let stiffness = f64::from(ctx.scalar_or_param("stiffness", 1.0).clamp(0.5, 3.0));
        let fitted = stiffness.min(stiffness_fitting_cap(dx, unit_wave, v_est));
        let limited = fitted < stiffness;
        if limited != self.limited {
            self.limited = limited;
            if limited {
                log::warn!(
                    "[matter] Stiffness {stiffness:.2} needs more than {MAX_SUBSTEPS} substeps at this resolution; running at {fitted:.2}"
                );
            }
        }
        let wave = (unit_wave * fitted) as f32;
        let body_limit = match &self.coupled.owner {
            Some(owner) => body_limit(owner, lattice.cell_size(), wave, v_est as f32)?,
            None => None,
        };
        let substeps = substeps_per_tick(lattice.cell_size(), wave, v_est as f32, body_limit, None).min(MAX_SUBSTEPS);
        let lambda = water_lambda(longest, fitted);
        let unit = momentum_unit(lattice.cell_size(), TICK / f64::from(substeps));
        let mut display_time = frame.display_time;
        if let Some(owner) = &mut self.coupled.owner {
            if frame.ticks > 0 {
                if (frame.ticks != 1 && !offline) || first_tick != owner.completed() {
                    return Err(format!(
                        "Matter coupling: fluid ticks {first_tick}+{} do not follow rigid tick {}",
                        frame.ticks,
                        owner.completed()
                    ));
                }
                let pending = PendingTick { tick: first_tick, stamp: clock.as_ref().map_or(0, FrameClock::stamp) };
                let scale = ReactionScale {
                    unit,
                    cell_size: lattice.cell_size(),
                    offset: self.bodies.count() - owner.rows().len(),
                };
                owner.set_pending(pending);
                self.coupled.scale = Some(scale);
                if frame.ticks > 1 {
                    self.coupled.exchange = Some(Exchange { substeps, ticks: frame.ticks, pending, scale });
                }
            }
            // The liquid is shown at the tick Box3D has settled, with the
            // bodies' accepted frame, which the scene takes before the region
            // runs this frame's ticks.
            display_time = owner.completed() as f64 * TICK;
        }
        self.coupled.transport = Some(ctx.time.seconds.0);
        let field = self
            .fields
            .prepare(FieldLattice::of(&lattice), self.acceleration.as_ref(), &frame, &self.impulses)?;

        let per_frame = [
            ("gravity_x", ctx.scalar_or_param("gravity_x", 0.0)),
            ("gravity", ctx.scalar_or_param("gravity", -9.81)),
            ("gravity_z", ctx.scalar_or_param("gravity_z", 0.0)),
            ("ticks", frame.ticks as f32),
            ("substeps_per_tick", substeps as f32),
            ("epoch", frame.epoch as f32),
            ("simulation_time", frame.simulation_time as f32),
            ("display_time", display_time as f32),
            ("dropped_seconds", frame.dropped_seconds as f32),
            ("lambda", lambda as f32),
            ("cohesion", ctx.scalar_or_param("cohesion", 0.0).clamp(0.0, 1.0)),
            ("liveliness", ctx.scalar_or_param("liveliness", 0.0).clamp(0.0, 1.0)),
            ("density", WATER_DENSITY),
            ("limited_by_substeps", if limited { fitted as f32 } else { 0.0 }),
            ("momentum_unit", unit),
            ("body_count", self.bodies.count() as f32),
            ("body_rows", rows),
            ("first_tick", first_tick as f32),
            ("dynamic_count", dynamic_count as f32),
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
    /// the liquid's clock, and build a new rigid owner (restarting the pair)
    /// on its reset or a change of bodies. False while the world's inputs are
    /// still pending.
    fn observe_rigid(&mut self, transport: f64, speed: f32) -> Result<bool, String> {
        let Coupling { observation, colliders, error, previous_reset, owner, owner_fresh, epochs, .. } = &mut self.coupled;
        if let Some(error) = error {
            return Err(error.clone());
        }
        let Some(observation) = observation.as_ref() else { return Ok(false) };
        if speed != observation.speed {
            return Err(format!(
                "Matter coupling: liquid and rigid bodies require the same Simulation Speed and shared World control (liquid {speed}, rigid {})",
                observation.speed
            ));
        }
        if (observation.transport.0 - transport).abs() > 1e-9 {
            return Err("Matter coupling: rigid observation transport does not match liquid transport".into());
        }
        CoupledRigidInputs { scene: &observation.inputs, colliders: *colliders, density: f64::from(WATER_DENSITY) }
            .validate()?;
        let reset_edge = previous_reset.is_some_and(|previous| previous != observation.reset);
        *previous_reset = Some(observation.reset);
        if reset_edge || owner.as_ref().is_none_or(|owner| !owner.matches(&observation.inputs, *colliders)) {
            *epochs += 1;
            *owner = Some(LiquidRigidOwner::new(&observation.inputs, *colliders, *epochs, owner.as_ref())?);
            *owner_fresh = true;
        }
        Ok(true)
    }

    /// Exchange before tick `tick` of this frame (counted from its first):
    /// the GPU has finished tick `tick − 1`, so its reaction words are final
    /// and the body rows are free to rewrite.
    fn exchange_tick(&mut self, tick: u32, exchange: Exchange) -> Result<(), String> {
        let observation = self.coupled.observation.as_ref().ok_or("Matter coupling: the rigid observation is missing")?;
        let owner = self.coupled.owner.as_mut().ok_or("Matter coupling: no coupled rigid world")?;
        let reaction = self.reaction.as_ref().ok_or("Matter coupling: the reaction array is missing")?;
        owner.settle(&observation.inputs, |_| true, |_, rows, impulses| {
            decode(exchange.scale, rows, reaction_words(Some(reaction)), impulses)
        })?;
        let (offset, rows) = self.bodies.set_coupled_rows(tick as usize, owner.rows())?;
        let bodies = &self.body_buffers.as_ref().ok_or("Matter coupling: the body rows are missing")?.bodies;
        let bytes: &[u8] = bytemuck::cast_slice(rows);
        if offset + bytes.len() as u64 > bodies.size {
            return Err("Matter coupling: the body rows outgrew their buffer".into());
        }
        // SAFETY: shared storage in bounds (checked above); the GPU has
        // completed every command that touched it, and the next reader is
        // encoded after this write.
        unsafe { bodies.write(offset, bytes) };
        reaction.zero_fill();
        owner.set_pending(PendingTick { tick: exchange.pending.tick + u64::from(tick), ..exchange.pending });
        Ok(())
    }

    /// A restart the liquid's own clock found (its reset, a backward seek):
    /// the rigid world restarts with it, keeping its hulls.
    fn rebuild_owner(&mut self) -> Result<(), String> {
        let observation = self.coupled.observation.as_ref().ok_or("Matter coupling: the rigid observation is missing")?;
        self.coupled.epochs += 1;
        let owner = LiquidRigidOwner::new(&observation.inputs, self.coupled.colliders, self.coupled.epochs, self.coupled.owner.as_ref())?;
        self.coupled.owner = Some(owner);
        Ok(())
    }
}
