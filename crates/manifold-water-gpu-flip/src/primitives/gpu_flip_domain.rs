//! `node.gpu_flip_domain` — the scene-facing CPU bridge of a GPU FLIP liquid
//! it speaks the shared liquid scene contract (names, types, meanings)
//! and turns it into the fixed-tick clock, the fill's sites, gravity, the
//! coupled bodies as body rows, shapes and a distance atlas, and the padded
//! authored lattice descriptor. The grid box sits on whole world cells (cell p
//! of the world centred on p·h), and `cell_origin` names the solver grid's
//! first cell in them. Each Inflow's own half-cell site box rides beside its
//! region row (`region_sites`). Separate mesh_min/mesh_nodes outputs carry the
//! native solver and surface grid for solid sampling; the particle frame
//! derives the same grid.
//!
//! Bounds is the Water item's box exactly as placed, with six faces that are
//! Wall (a solid) or Open (a removal plane); with Bounds off there is no box
//! to hold the water, and water below the kill height is removed. The Wall and
//! Open masks, the box and the kill height are published every frame. Scene
//! forces and impulses reach the water through the shared field lattices of
//! `liquid::fields` (seam P8), laid over the scene box: the Bounds box, or
//! with Bounds off the box round every body, region and the Initial Fill.
//! Paired with a physics world, the scene's Box3D bodies join the water two
//! ways. The domain owns the rigid world in-thread and steps it one settled
//! tick at a time with the steps' reaction. Every lattice atom reads the
//! lattice from its wires, so Cell Size applies on change. Exempt from the
//! codegen mandate as a CPU bridge (ADDING_PRIMITIVES.md exclusion 3).

use std::borrow::Cow;

use manifold_gpu::{FrameClock, GpuBuffer};
use manifold_nodes_physics::force_lattice::felt_field;
use manifold_physics::FieldValue;

use super::gpu_flip_pressure::{MAX_ITERATIONS, lattice_refusal};
use super::gpu_flip_step::{
    pool_tiles, read_max_iterations, read_sheet_fill_rate, read_solve_level,
};
use super::liquid_fill::{SITES_PER_CELL, filled_sites, site_range};
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_core::fluid_domain::{
    CELL_SIZE_METRES, DEFAULT_CELL_SIZE, FluidDomainLayout, domain_layout,
};
use manifold_core::liquid_setup::{
    ColumnOutcome, DEFAULT_FILL_HEIGHT, FillFit, SetupFit, fit_column, fit_fill_height,
};
use manifold_core::physics_gizmo::{PhysicsGizmo, PhysicsGizmoFamily};
use manifold_core::scene_impulse::RigidImpulseTargets;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::ports::EXACT_F32_COUNT;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::transform::Transform;
use manifold_nodes_physics::tick_sampled::{TickSampled, TickSampledRegistration};
use manifold_physics::clock::TICK;
use manifold_physics::tick_samples::TickSamples;
use manifold_water_liquid::bodies::{BodiesStatus, LiquidBodies, LiquidBody, LiquidShape};
use manifold_water_liquid::body_buffers::LiquidBodyBuffers;
use manifold_water_liquid::clock::LiquidClock;
use manifold_water_liquid::coupling::{
    DomainWalls, LiquidRigidOwner, PendingTick, REACTION_FLOATS, SharedSolids, SharesSolids,
    decode_reaction, takes_reaction,
};
use manifold_water_liquid::fluid_role::{FluidRole, FluidRoleKind, take_initial_fill};
use manifold_water_liquid::gizmos::SetupReport;
use manifold_water_liquid::lattice::closed_faces;
use manifold_water_liquid::lattice::{FlipSolverGrid, LiquidLattice};
use manifold_water_liquid::{RolePorts, WATER_DENSITY};
use manifold_water_rigid::coupled_frame::{CoupledRigidFrame, CoupledRigidInputs};
use manifold_water_rigid::node::{PhysicsNode, PhysicsNodeRegistration};
use manifold_water_rigid::physics::{RigidSceneInputs, RigidSceneObservation};
use manifold_water_rigid::physics_events::ResolvedNodeImpulse;
use region_sites::{RegionSiteBuffer, RegionSiteRows};
use {
    manifold_physics::force_lattice::ForceLattice, manifold_water_liquid::fields,
    manifold_water_liquid::fields::LiquidFields, manifold_water_liquid::fields::LiquidImpulses,
};

mod region_sites;

manifold_core::testkit_visible! {
/// Everything whose change restarts the liquid. The Bounds box's pose and
/// size are not in it: moving or resizing the box moves the walls under the
/// running water.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuFlipSetup {
    /// The cell size in metres.
    pub cell_size: f64,
    /// The Bounds switch: turning it on or off restarts the liquid.
    pub bounds: bool,
    /// The authored Fill Height in metres.
    pub fill_height: f32,
    /// The Initial Fill's box as placed (min then max), None without one.
    pub initial_fill: Option<[[f32; 3]; 2]>,
    /// Slots past the fill's count the pool holds: 0 keeps the fill's count;
    /// an Inflow gets room for the whole domain up to a memory budget
    /// ([`GpuFlipGeometry::with_inflow`]). Sources emit into the slots past
    /// the live particles.
    pub(crate) particle_capacity: u32,
}
}

manifold_core::testkit_visible! {
/// The domain's setup and the layout it came from, computed from params and
/// wires alone: the node and the extent checker both call [`gpu_flip_geometry`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuFlipGeometry {
    pub(crate) layout: FluidDomainLayout,
    /// The Water item's box exactly as placed (min then max), not moved onto
    /// whole cells: the Bounds box.
    pub(crate) bounds_box: [[f32; 3]; 2],
    pub setup: GpuFlipSetup,
    /// The padded lattice over the grid box of this frame.
    pub lattice: LiquidLattice,
    /// The pool's height in half-cell sites of this frame's grid box.
    pub pool_sites: u32,
    /// The Initial Fill's site range per axis in this frame's grid box (empty
    /// without one).
    pub box_sites: [[u32; 2]; 3],
    /// What the fill changed to fit the domain, on the values this frame ran.
    pub fit: SetupFit,
    pub particles: u64,
    /// The V-cycle level the pressure solves run on; live, so not in the
    /// setup.
    pub(crate) solve_level: usize,
    /// The cap Auto pressure solves converge within; live, so not in the setup.
    pub(crate) max_iterations: u32,
    /// Live sheet seeding rate; changing it does not restart the liquid.
    pub(crate) sheet_fill_rate: f32,
}
}

/// The world cell a box minimum corner starts at: the nearest whole cell.
fn box_cell(min: f32, cell_size: f64) -> i32 {
    (f64::from(min) / cell_size).round() as i32
}

/// `layout` with its minimum corner on a whole world cell. Cell p of the world
/// is centred on p·h, so a box that starts on one has its cells on the world
/// lattice and its fill sites at fixed world positions, whatever the domain's
/// pose. The corner moves by less than half a cell; the size and the cell
/// count stay.
fn on_world_cells(layout: FluidDomainLayout) -> FluidDomainLayout {
    let h = layout.cell_size;
    FluidDomainLayout {
        min: layout.min.map(|v| (f64::from(box_cell(v, h)) * h) as f32),
        ..layout
    }
}

impl GpuFlipGeometry {
    /// The world cell of the solver grid's first cell: the solver grid starts
    /// one cell below the box's first.
    pub(crate) fn cell_origin(&self) -> [i32; 3] {
        self.layout
            .min
            .map(|v| box_cell(v, self.layout.cell_size) - 1)
    }

    manifold_core::testkit_visible! {
        /// The scalar outputs fixed by the setup, by name.
        pub(crate) fn outputs(&self) -> [(&'static str, f32); 25] {
            let GpuFlipGeometry { lattice, pool_sites, box_sites, .. } = *self;
            let particle_capacity = self.setup.particle_capacity;
            let h = self.layout.cell_size;
            let surface = lattice.surface();
            let origin = self.cell_origin();
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
                ("cell_origin_x", origin[0] as f32),
                ("cell_origin_y", origin[1] as f32),
                ("cell_origin_z", origin[2] as f32),
            ]
        }
    }
}

/// The fill in the layout's half-cell sites: the pool's height and the
/// Initial Fill's site range per axis (empty without one). A height past
/// the top is capped there and a volume past a wall is clipped to the domain;
/// one wholly outside fills nothing (`manifold_core::liquid_setup`, which the
/// setup check reports from). An unusable height or a volume that is not an
/// axis-aligned finite box is refused by name.
fn fill_sites(
    layout: &FluidDomainLayout,
    fill_height: f32,
    initial_fill: Option<Transform>,
) -> Result<(u32, [[u32; 2]; 3], SetupFit), String> {
    let filled =
        fit_fill_height(layout, fill_height).map_err(|reason| format!("GPU FLIP: {reason}"))?;
    let mut fit = SetupFit {
        fill_capped: matches!(filled, FillFit::Capped(_)),
        column: ColumnOutcome::Fits,
        pool_full: false,
        tile_removed: 0,
    };
    let fill_height = filled.height();
    let min = layout.min.map(f64::from);
    let h = layout.cell_size;
    let pool = site_range(
        min[1],
        min[1] + f64::from(fill_height),
        min[1],
        h,
        layout.cells[1],
    )[1];
    let mut sites = [[0u32; 2]; 3];
    if let Some(volume) = initial_fill {
        if volume.billboard
            || volume
                .rot_euler
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 1e-6)
        {
            return Err("GPU FLIP: the Initial Fill must be an axis-aligned box; rotation and billboarding are not supported".into());
        }
        let (edges, outcome) = fit_column(layout, volume.pos, volume.scale)
            .map_err(|reason| format!("GPU FLIP: {reason}"))?;
        fit.column = outcome;
        if let Some((lo, hi)) = edges {
            for (d, range) in sites.iter_mut().enumerate() {
                *range = site_range(lo[d], hi[d], min[d], h, layout.cells[d]);
            }
        }
    }
    Ok((pool, sites, fit))
}

manifold_core::testkit_visible! {
/// The domain box, lattice and fill from the domain's params and wires
/// (`read` is `scalar_or_param`, with the Bounds switch as 0 or 1: see
/// [`geometry_read`]), the grid box moved onto whole world cells
/// ([`on_world_cells`]) and the Bounds box as placed. Refused by name, in this
/// order: a lattice the pressure solve cannot take, a solve level the lattice
/// lacks, a Fill Height above 0 with Bounds off, a fill that is not a usable
/// shape, and a fill past the count a wire carries exactly.
pub fn gpu_flip_geometry(
    read: impl Fn(&str, f32) -> f32,
    domain: Option<Transform>,
    initial_fill: Option<Transform>,
) -> Result<GpuFlipGeometry, String> {
    let bounds = read("bounds", 1.0) != 0.0;
    let cell_size = read("cell_metres", DEFAULT_CELL_SIZE);
    let domain_size = read("domain_size", 4.0);
    let world = domain.map(manifold_water_liquid::world_pose::world_domain_box);
    let layout = on_world_cells(domain_layout(world.map(Into::into), domain_size, f64::from(cell_size))?);
    let bounds_box = match world {
        Some(pose) => box_about(pose.pos, pose.scale),
        // No Water item: the cube `domain_layout` places, centred in X/Z with
        // its floor at Y = 0.
        None => box_about([0.0, 0.5 * domain_size, 0.0], [domain_size; 3]),
    };
    let solver = FlipSolverGrid::from_lattice(LiquidLattice::from_layout(&layout));
    if let Some(reason) = lattice_refusal(solver.cells()) {
        return Err(format!("GPU FLIP: {reason}. Raise Cell Size."));
    }
    let solve_level = read_solve_level(read("solve_level", 0.0), pool_tiles(solver.cells())).map_err(|reason| format!("GPU FLIP: {reason}"))?;
    let max_iterations = read_max_iterations(read("max_iterations", MAX_ITERATIONS as f32)).map_err(|reason| format!("GPU FLIP: {reason}"))?;
    let sheet_fill_rate = read_sheet_fill_rate(read("sheet_fill_rate", 0.0)).map_err(|reason| format!("GPU FLIP: {reason}"))?;
    let fill_height = read("fill_height", DEFAULT_FILL_HEIGHT);
    if !bounds && fill_height > 0.0 {
        return Err("GPU FLIP: Fill Height fills the Bounds box from its floor, so with Bounds off it must be 0. Use an Initial Fill for the starting water.".into());
    }
    let (pool_sites, box_sites, fit) = fill_sites(&layout, fill_height, initial_fill)?;
    let particles = filled_sites(layout.cells, pool_sites, box_sites);
    if particles > u64::from(EXACT_F32_COUNT) {
        return Err(format!(
            "GPU FLIP: the pool holds {particles} particles, more than the {EXACT_F32_COUNT} a particle count carries exactly. Raise Cell Size or lower Fill Height."
        ));
    }
    let setup = GpuFlipSetup {
        cell_size: layout.cell_size,
        bounds,
        fill_height,
        initial_fill: initial_fill.map(|volume| box_about(volume.pos, volume.scale)),
        particle_capacity: 0,
    };
    let lattice = LiquidLattice::from_layout(&layout);
    Ok(GpuFlipGeometry { layout, bounds_box, setup, lattice, pool_sites, box_sites, particles, solve_level, max_iterations, sheet_fill_rate, fit })
}
}

/// The box of `size` centred on `pos` (min then max).
fn box_about(pos: [f32; 3], size: [f32; 3]) -> [[f32; 3]; 2] {
    [
        std::array::from_fn(|i| pos[i] - 0.5 * size[i]),
        std::array::from_fn(|i| pos[i] + 0.5 * size[i]),
    ]
}

/// The Bounds switch of `params`: on unless the toggle is off.
pub(crate) fn bounds_enabled(params: &ParamValues) -> bool {
    !matches!(params.get("bounds"), Some(ParamValue::Bool(false)))
}

/// `scalar` (the wire, else a Float param, else the default) as
/// [`gpu_flip_geometry`] reads it, plus the Bounds switch, a Bool param that
/// `scalar` does not carry.
pub(crate) fn geometry_read<'a>(
    params: &'a ParamValues,
    scalar: impl Fn(&str, f32) -> f32 + 'a,
) -> impl Fn(&str, f32) -> f32 + 'a {
    move |name, default| {
        if name == "bounds" {
            f32::from(bounds_enabled(params))
        } else {
            scalar(name, default)
        }
    }
}

/// All six faces, bits −X +X −Y +Y −Z +Z (the order of `closed_faces`).
const ALL_FACES: u32 = 0b11_1111;

/// The Wall and Open masks of a frame's faces: `closed` holds the Closed
/// toggles, a closed face being Wall and an open one Open. With Bounds off
/// there are no faces, so neither mask has a bit.
fn face_masks(bounds: bool, closed: u32) -> (u32, u32) {
    if bounds {
        (closed & ALL_FACES, ALL_FACES & !closed)
    } else {
        (0, 0)
    }
}

/// The kill height Bounds on publishes, below every particle: nothing reads it.
const NO_KILL_HEIGHT: f32 = f32::MIN;

/// The radius of a sphere of an eighth of a cell, in cells: what a particle
/// of the fill and of every source is drawn and kept with.
const PARTICLE_RADIUS_CELLS: f32 = 0.31017524;

/// The kill height (m) with Bounds off: the lowest solid point minus
/// `drawn_radius`. The lowest solid point is the lowest world-box minimum y
/// over the Fixed bodies, `bodies` being (is Fixed, box minimum y); with no
/// Fixed body, over every body; with no body, `source_floor`, the lowest
/// Initial Fill or Inflow when the epoch began. A body that falls forever
/// must not drag the plane down with it, which is why Fixed bodies come first.
/// With nothing placed there is nothing to fall past: [`NO_KILL_HEIGHT`].
fn kill_height(
    bodies: impl IntoIterator<Item = (bool, f32)>,
    source_floor: Option<f32>,
    drawn_radius: f32,
) -> f32 {
    let lowest = |a: Option<f32>, y: f32| Some(a.map_or(y, |a| a.min(y)));
    let (fixed, any) = bodies
        .into_iter()
        .fold((None, None), |(fixed, any), (is_fixed, y)| {
            (
                if is_fixed { lowest(fixed, y) } else { fixed },
                lowest(any, y),
            )
        });
    match fixed.or(any).or(source_floor) {
        Some(floor) => floor - drawn_radius,
        None => NO_KILL_HEIGHT,
    }
}

/// The box round `boxes`, or None when nothing is placed or the box has no
/// finite extent.
fn union_box(boxes: impl IntoIterator<Item = [[f32; 3]; 2]>) -> Option<[[f32; 3]; 2]> {
    let union = boxes.into_iter().reduce(|a, b| {
        [
            std::array::from_fn(|i| a[0][i].min(b[0][i])),
            std::array::from_fn(|i| a[1][i].max(b[1][i])),
        ]
    })?;
    let longest = (0..3)
        .map(|i| union[1][i] - union[0][i])
        .fold(0.0, f32::max);
    (longest.is_finite() && longest > 0.0).then_some(union)
}

/// The six Bounds outputs for the box `scene`.
fn bounds_outputs(scene: [[f32; 3]; 2]) -> [(&'static str, f32); 6] {
    [
        ("bounds_lo_x", scene[0][0]),
        ("bounds_lo_y", scene[0][1]),
        ("bounds_lo_z", scene[0][2]),
        ("bounds_hi_x", scene[1][0]),
        ("bounds_hi_y", scene[1][1]),
        ("bounds_hi_z", scene[1][2]),
    ]
}

/// The Bounds faces' velocities along their axes (m/s): `now` minus
/// `previous` over `seconds` of simulated time. 0 without a previous box (the
/// first frame, a restart, Bounds off) and when no time ran (a paused drag is
/// a teleport).
fn wall_velocity_outputs(
    previous: Option<[[f32; 3]; 2]>,
    now: [[f32; 3]; 2],
    seconds: f64,
) -> [(&'static str, f32); 6] {
    let speed = |end: usize, axis: usize| match previous {
        Some(before) if seconds > 0.0 => {
            ((f64::from(now[end][axis]) - f64::from(before[end][axis])) / seconds) as f32
        }
        _ => 0.0,
    };
    [
        ("wall_velocity_lo_x", speed(0, 0)),
        ("wall_velocity_lo_y", speed(0, 1)),
        ("wall_velocity_lo_z", speed(0, 2)),
        ("wall_velocity_hi_x", speed(1, 0)),
        ("wall_velocity_hi_y", speed(1, 1)),
        ("wall_velocity_hi_z", speed(1, 2)),
    ]
}

/// The lowest Initial Fill or Inflow when an epoch began: the fill's lower
/// edge and each Inflow's box minimum y (`inflows`).
fn lowest_source(fill: Option<Transform>, inflows: impl IntoIterator<Item = f32>) -> Option<f32> {
    fill.map(|fill| fill.pos[1] - 0.5 * fill.scale[1])
        .into_iter()
        .chain(inflows)
        .reduce(f32::min)
}

/// What an epoch started with: the kill height hangs from it while no body is
/// placed. Taken once per epoch, so a source that moves later does not move
/// the plane.
#[derive(Default)]
pub struct SourceFloor {
    epoch: Option<u32>,
    y: Option<f32>,
}

/// The most slots the pool gives an inflow: about 1 GB of device memory,
/// since a slot costs the rendered water about 1 kB across its particle
/// buffers (the extent walk's count at 64³). It holds half a 64³ domain.
pub const AUTO_INFLOW_SLOTS: u32 = 1 << 20;

/// The kinds of the roles a frame read. Presence, not Enabled, so switching
/// an inflow on and off live never resizes the pool, which would restart the
/// water.
fn role_kinds(roles: &[Option<FluidRole>]) -> impl Iterator<Item = FluidRoleKind> + '_ {
    roles.iter().flatten().map(|role| role.kind)
}

impl GpuFlipGeometry {
    /// A domain with an Inflow holds one slot per half-cell site of the
    /// domain, the sites the fill and an inflow seed at, up to
    /// [`AUTO_INFLOW_SLOTS`]. An inflow fills only empty sites, so in a
    /// domain within that it flows until the domain is full of water, as FLIP
    /// Fluids' unbounded particle list does; a Drain or an open face frees
    /// slots for it again. A pool that still fills is reported, never silent
    /// (stats word 28 into the setup check).
    pub fn with_inflow(mut self) -> Self {
        let sites = self
            .layout
            .cells
            .iter()
            .map(|&n| 2 * u64::from(n))
            .product::<u64>()
            .min(u64::from(AUTO_INFLOW_SLOTS));
        self.setup.particle_capacity = sites as u32;
        self.particles = self.particles.max(sites);
        self
    }

    /// The pool the wired role kinds need: room for an inflow when any is
    /// one. The node and its extent rule both size through here.
    pub(crate) fn sized_for(self, kinds: impl IntoIterator<Item = FluidRoleKind>) -> Self {
        if kinds.into_iter().any(|kind| kind == FluidRoleKind::Inflow) {
            self.with_inflow()
        } else {
            self
        }
    }

    /// The field lattice over the scene box `scene` (min then max): at most
    /// as many nodes along the longest side as the lattice over the face grid
    /// has, so the scene's forces and impulses cost what they always did
    /// whatever the box's size.
    pub(crate) fn field_lattice(&self, scene: [[f32; 3]; 2]) -> ForceLattice {
        let solver = FlipSolverGrid::from_lattice(self.lattice);
        let detail =
            ForceLattice::covering(solver.min(), self.layout.cell_size as f32, solver.nodes())
                .nodes()
                .into_iter()
                .fold(0, u32::max);
        ForceLattice::over_box(
            scene[0],
            std::array::from_fn(|i| scene[1][i] - scene[0][i]),
            detail,
        )
    }

    /// The scene box the memory walk sizes the field lattices for: the Bounds
    /// box, or with Bounds off, where the bodies set the box and the walk has
    /// none, a unit cube, which takes the most nodes any box does.
    #[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
    pub(crate) fn walk_scene(&self) -> [[f32; 3]; 2] {
        if self.setup.bounds {
            self.bounds_box
        } else {
            [
                self.bounds_box[0],
                std::array::from_fn(|i| self.bounds_box[0][i] + 1.0),
            ]
        }
    }

    /// The rigid world's walls for the Wall faces `walls`: slabs round the
    /// Bounds box, and none with Bounds off.
    pub(crate) fn domain_walls(&self, walls: u32) -> DomainWalls {
        if self.setup.bounds {
            DomainWalls::new(self.bounds_box[0], self.bounds_box[1], walls)
        } else {
            DomainWalls::default()
        }
    }
}

/// Every scalar output, in the order [`GpuFlipDomain::compute`] fills them.
const OUTPUTS: [&str; 73] = [
    "lattice_min_x",
    "lattice_min_y",
    "lattice_min_z",
    "cell_size",
    "nodes_x",
    "nodes_y",
    "nodes_z",
    "closed_faces",
    "pool_sites",
    "box_x0",
    "box_x1",
    "box_y0",
    "box_y1",
    "box_z0",
    "box_z1",
    "particle_mass",
    "gravity_x",
    "gravity",
    "gravity_z",
    "ticks",
    "epoch",
    "simulation_time",
    "display_time",
    "target_time",
    "dropped_seconds",
    "body_count",
    "body_rows",
    "first_tick",
    "field_nodes_x",
    "field_nodes_y",
    "field_nodes_z",
    "field_spacing",
    "force_lattices",
    "impulse_tick",
    "dynamic_bodies",
    "particle_capacity",
    "region_count",
    "solve_level",
    "interval_duration",
    "limit_interval",
    "clock_obstacle_count",
    "clock_source_count",
    "live_hit_count",
    "mesh_min_x",
    "mesh_min_y",
    "mesh_min_z",
    "mesh_nodes_x",
    "mesh_nodes_y",
    "mesh_nodes_z",
    "max_iterations",
    "display_cursor",
    "sheet_fill_rate",
    "handover_position",
    "handover_velocity",
    "handover_rotation",
    "cell_origin_x",
    "cell_origin_y",
    "cell_origin_z",
    "region_site_total",
    "bounds_lo_x",
    "bounds_lo_y",
    "bounds_lo_z",
    "bounds_hi_x",
    "bounds_hi_y",
    "bounds_hi_z",
    "open_faces",
    "kill_height",
    "wall_velocity_lo_x",
    "wall_velocity_lo_y",
    "wall_velocity_lo_z",
    "wall_velocity_hi_x",
    "wall_velocity_hi_y",
    "wall_velocity_hi_z",
];
const TICKS: usize = 19;
const EPOCH: usize = 20;
const IMPULSE_TICK: usize = 33;
const _: () = assert!(matches!(OUTPUTS[TICKS].as_bytes(), b"ticks"));
const _: () = assert!(matches!(OUTPUTS[EPOCH].as_bytes(), b"epoch"));
const _: () = assert!(matches!(OUTPUTS[IMPULSE_TICK].as_bytes(), b"impulse_tick"));

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
    /// This frame's Bounds box and Wall faces: the bodies' walls.
    walls: DomainWalls,
    /// The coupled bodies the liquid has body rows for.
    capacity: usize,
    /// This frame's simulation step, handed to the rigid owner.
    step: manifold_water_rigid::physics::SimStep,
    /// Rows before the pending tick's first coupled body's, so its
    /// reaction decodes against the bodies it ran with.
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
        *self = Self {
            mode,
            epochs,
            ..Self::default()
        };
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

manifold_node_engine::primitive! {
    name: GpuFlipDomain,
    type_id: "node.gpu_flip_domain",
    purpose: "Define a GPU FLIP liquid domain with the shared liquid scene contract: the axis-aligned domain box, cell size, initial fill height and the box an Initial Fill role places, gravity, the scene's acceleration field and impulses, simulation speed and reset. Outputs this frame's fixed 60 Hz ticks, the epoch and the display clock, the fill's half-cell sites and particle mass, gravity, the scene's forces and impulses on coarse field lattices over the scene box, and the padded lattice for the step, the solid distance and the particle frame. Bounds is the Water item's box exactly as placed (bounds_lo_x/y/z, bounds_hi_x/y/z in metres) with six faces: a face is a Wall (closed_faces, bit 2d the low face of axis d, bit 2d + 1 the high one) unless its Closed param is off, which makes it Open (open_faces, same bits), where the water that reaches it is removed. Bounds off has no box to hold the water: both masks are 0, and kill_height is the height (metres) below which water is removed, the lowest Fixed body's lowest point (else the lowest body's, else the lowest Initial Fill or Inflow when the epoch began) minus drawn_radius, which defaults to the particle radius; with Bounds on kill_height is the lowest float and the scene box is the Bounds box. A Fill Height above 0 with Bounds off is refused. A hit fired while the liquid is held (paused, Speed 0) is discarded, never replayed. Its Sources param sets how many role inputs (role_0, role_1, ...) it takes. Paired with a physics world, its bodies join the water two ways: the domain steps the world one settled 1/60 s tick at a time, each body is one row per tick with a shape and a distance lattice packed in one half-precision atlas, the water flowing around it, dynamic_bodies counts the ones the water pushes, and the reaction the steps add up reaches each body as one impulse per tick. Live, the pair runs at most one tick per frame and holds while that tick's reaction is still on the GPU. Inflow and Outflow roles become region rows (regions, region_count) beside the body rows, sharing the shapes and atlas: an inflow emits water at its velocity into its volume each substep, an outflow removes the water inside it. Beside each region row region_sites carries the half-cell sites its inflow can reach during the tick (an Outflow's and a disabled row's are empty), eight words a row, and region_site_total the most sites any tick's inflows hold together. The domain box is moved onto whole world cells (cell p of the world centred on p·h, under half a cell from where it was authored), and cell_origin_x/y/z name the solver grid's first cell in them. Particle Capacity sizes the particle pool sources emit into (0 = the fill's count); a full pool stops emitting and shows in the stats. An Initial Fill role is the water's starting box and never a body; a second Initial Fill switched on is refused by name.",
    inputs: {
        domain: Transform optional,
        acceleration_field: VectorField optional,
        cell_metres: ScalarF32 optional,
        domain_size: ScalarF32 optional,
        fill_height: ScalarF32 optional,
        drawn_radius: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        speed: ScalarF32 optional,
        air_drag: ScalarF32 optional,
        air_resistance: ScalarF32 optional,
        reset: ScalarF32 optional,
    },
    outputs: {
        lattice_min_x: ScalarF32, lattice_min_y: ScalarF32, lattice_min_z: ScalarF32,
        mesh_min_x: ScalarF32, mesh_min_y: ScalarF32, mesh_min_z: ScalarF32,
        mesh_nodes_x: ScalarF32, mesh_nodes_y: ScalarF32, mesh_nodes_z: ScalarF32,
        cell_size: ScalarF32,
        nodes_x: ScalarF32, nodes_y: ScalarF32, nodes_z: ScalarF32,
        closed_faces: ScalarF32, open_faces: ScalarF32,
        bounds_lo_x: ScalarF32, bounds_lo_y: ScalarF32, bounds_lo_z: ScalarF32,
        bounds_hi_x: ScalarF32, bounds_hi_y: ScalarF32, bounds_hi_z: ScalarF32,
        wall_velocity_lo_x: ScalarF32, wall_velocity_lo_y: ScalarF32, wall_velocity_lo_z: ScalarF32,
        wall_velocity_hi_x: ScalarF32, wall_velocity_hi_y: ScalarF32, wall_velocity_hi_z: ScalarF32,
        kill_height: ScalarF32,
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
        cell_origin_x: ScalarF32, cell_origin_y: ScalarF32, cell_origin_z: ScalarF32,
        region_site_total: ScalarF32,
        bodies: Array(LiquidBody), regions: Array(LiquidBody), region_sites: Array(u32), shapes: Array(LiquidShape), atlas: Array(u32),
        reaction: Array(f32),
        contacts: Array(f32),
        forces: Array(f32), impulses: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("cell_metres"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(DEFAULT_CELL_SIZE), range: Some(CELL_SIZE_METRES), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("solve_level"), label: "Coarse Solve", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("max_iterations"), label: "Max Iterations", ty: ParamType::Int, default: ParamValue::Float(MAX_ITERATIONS as f32), range: Some((1.0, MAX_ITERATIONS as f32)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("sheet_fill_rate"), label: "Sheet Fill Rate", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("domain_size"), label: "Domain Size", ty: ParamType::Float, default: ParamValue::Float(4.0), range: Some((0.5, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_height"), label: "Fill Height", ty: ParamType::Float, default: ParamValue::Float(0.4), range: Some((0.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("speed"), label: "Speed", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("air_drag"), label: "Air Drag", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("reset"), label: "Reset", ty: ParamType::Trigger, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds"), label: "Bounds", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_x"), label: "Left Wall", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_x"), label: "Right Wall", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_y"), label: "Floor", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_y"), label: "Ceiling", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_z"), label: "Back Wall", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_z"), label: "Front Wall", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("roles"), label: "Sources", ty: ParamType::Int, default: ParamValue::Float(manifold_water_liquid::DEFAULT_ROLE_PORTS as f32), range: None, enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "The GPU FLIP group's source of truth: ticks and epoch into node.liquid_state (the tick region's clock owner), the fill sites, the padded lattice, bounds_lo/hi and closed_faces into node.liquid_fill, gravity, forces, impulses, the field scalars (field_nodes_x/y/z, field_spacing, force_lattices, first_tick, impulse_tick), bodies, regions, region_count, shapes, atlas, dynamic_bodies, the padded lattice, bounds_lo/hi, closed_faces, open_faces and kill_height into every node.gpu_flip_step, reaction into the first step of the tick (each later step takes the one before's reaction_out), and the padded lattice with bodies, shapes and atlas into node.liquid_solid_distance (which also takes bounds_lo/hi and closed_faces), node.whitewater_obstacle_source (the same), node.liquid_frame and node.face_sample_component (which also takes cell_origin_x/y/z); region_sites and region_site_total into every node.gpu_flip_step for its emission; simulation_time, display_time and epoch into node.liquid_frame; particle_mass into node.liquid_stats; particle_capacity into node.liquid_fill. The domain box, Resolution, Bounds and fill restart the liquid; gravity and Simulation Speed are live. Live runs at most two fixed Sim Rate intervals per display frame and reports dropped time; export runs every interval.",
    examples: [],
    picker: { label: "GPU FLIP Domain", category: Atom },
    summary: "Sets up a GPU FLIP liquid: its box, cell size, starting fill, gravity and speed.",
    category: Particles3D,
    role: Source,
    aliases: ["gpu flip", "gpu water", "liquid domain"],
    boundary_reason: NonGpu,
    extra_fields: {
        clock: LiquidClock = LiquidClock::default(),
        scheduled_frame: Option<manifold_physics::clock::ClockFrame> = None,
        setup: Option<GpuFlipSetup> = None,
        // What the setup this frame ran capped or clipped, or why it was
        // refused; it rides the domain snapshot.
        setup_report: SetupReport = SetupReport::default(),
        // The domain box of the accepted setup.
        layout: Option<FluidDomainLayout> = None,
        // The pool height and the Initial Fill's site range of the grid box
        // this frame ran (the fill gizmos).
        fill_sites: (u32, [[u32; 2]; 3]) = (0, [[0; 2]; 3]),
        // Last frame's Bounds box, None with Bounds off: the walls' velocity
        // is this frame's box against it.
        last_bounds: Option<[[f32; 3]; 2]> = None,
        // The box the force and impulse lattices cover this frame (the scene box).
        scene_box: [[f32; 3]; 2] = [[0.0; 3]; 2],
        // The lowest Initial Fill or Inflow of the current epoch, with Bounds off.
        source_floor: SourceFloor = SourceFloor::default(),
        published: Option<[f32; OUTPUTS.len()]> = None,
        bodies: LiquidBodies = LiquidBodies::with_regions(),
        // The role inputs, one per source: the `roles` param sizes them.
        role_ports: RolePorts = RolePorts::default(),
        // Another water's solids, when this one does not own the world.
        observed: SharedSolids = SharedSolids::default(),
        body_buffers: LiquidBodyBuffers = LiquidBodyBuffers::default(),
        // The Inflows' site boxes beside the region rows, and their buffer.
        region_sites: RegionSiteRows = RegionSiteRows::default(),
        region_site_buffer: RegionSiteBuffer = RegionSiteBuffer::default(),
        role_pending: bool = false,
        body_rows: f32 = 0.0,
        rows_fresh: bool = false,
        coupled: Coupling = Coupling::default(),
        reaction: Option<GpuBuffer> = None,
        impulses: LiquidImpulses = LiquidImpulses::default(),
        fields: LiquidFields = LiquidFields::default(),
        acceleration: Option<FieldValue> = None,
        holding: bool = false,
    },
}

impl Primitive for GpuFlipDomain {
    fn reconfigure(&mut self, params: &ParamValues) {
        self.role_ports.configure(
            <Self as manifold_node_engine::primitive::PrimitiveSpec>::INPUTS,
            params,
        );
    }

    fn live_inputs(&self) -> Option<&[manifold_node_engine::ports::NodeInput]> {
        self.role_ports.inputs()
    }

    /// A liquid follows the Physics World: its gravity and its clock.
    fn world_control_inputs(
        &self,
    ) -> &'static [manifold_core::scene_world_controls::WorldControlInput] {
        &manifold_core::scene_world_controls::WORLD_INPUTS
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(
            port,
            "bodies"
                | "contacts"
                | "regions"
                | "region_sites"
                | "shapes"
                | "atlas"
                | "reaction"
                | "forces"
                | "impulses"
                | "clock_obstacles"
                | "clock_sources"
                | "live_hits"
        )
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "reaction" => return self.reaction.as_ref(),
            "forces" => return self.fields.forces_buffer(),
            "impulses" => return self.fields.impulses_buffer(),
            "live_hits" => return self.fields.live_hits_buffer(),
            "region_sites" => return self.region_site_buffer.buffer(),
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
        matches!(
            port_name,
            "bodies"
                | "contacts"
                | "regions"
                | "region_sites"
                | "shapes"
                | "atlas"
                | "reaction"
                | "forces"
                | "impulses"
                | "clock_obstacles"
                | "clock_sources"
                | "live_hits"
        )
        .then_some(1)
    }

    fn warmup_pending(&self) -> bool {
        self.role_pending
    }

    fn substep_clock_interval(
        &self,
        iteration: u32,
    ) -> Option<manifold_node_engine::exec::substeps::SubstepClockOutput<'_>> {
        let frame = self.scheduled_frame.as_ref()?;
        frame.interval(u64::from(iteration)).map(|interval| {
            manifold_node_engine::exec::substeps::SubstepClockOutput::single(
                "interval_duration",
                interval.start,
                interval.end,
                iteration,
                frame.ticks,
            )
        })
    }

    /// Coupled frames sync before each later tick of the frame.
    fn substep_host_sync(&self, iteration: u32) -> bool {
        self.coupled
            .exchange
            .as_ref()
            .is_some_and(|x| iteration < x.ticks)
    }

    /// Between two ticks of one frame: settle the tick the GPU just
    /// finished, step Box3D over it, and hand the next tick the bodies' new
    /// state and a cleared reaction.
    fn substep_host_step(
        &mut self,
        iteration: u32,
        _gpu: Option<&mut manifold_node_engine::gpu::gpu_encoder::GpuEncoder<'_>>,
    ) -> Result<(), String> {
        let Some(exchange) = self.coupled.exchange.clone() else {
            return Ok(());
        };
        let result = self.exchange_tick(iteration, exchange);
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

    // While an input is pending the liquid holds its last good frame.
    fn runs_with_pending_inputs(&self) -> bool {
        true
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.clock
            .set_live_load(manifold_water_rigid::physics::live_load(ctx.sim_step));
        self.clock.set_played_target(ctx.sim_step.played_target);
        self.coupled.step = ctx.sim_step;
        if let Some(owner) = self.coupled.owner.as_mut() {
            owner.set_step(ctx.sim_step);
        }
        let role_pending = self.role_ports.read(&ctx.inputs);
        let mut roles = self.role_ports.take();
        // The Initial Fill seeds the water; the bodies never see it.
        let fill = take_initial_fill(&mut roles);
        // A physics sample reads the force field and the roles at a tick's
        // start; it never advances time.
        if ctx.sim_step.authored_sample_only {
            self.clock
                .observe_speed(ctx.played(), ctx.scalar_or_param("speed", 1.0));
            match felt_field(ctx, "Liquid") {
                Ok(field) => self
                    .fields
                    .observe_sample(&self.clock, ctx.played(), field.as_ref()),
                Err(error) => ctx.error(error),
            }
            self.bodies.observe_sample(
                &self.clock,
                ctx.played(),
                (!role_pending).then_some(&roles[..]),
            );
            self.coupled
                .scenes
                .observe_staged(&self.clock, ctx.played());
            self.role_ports.put_back(roles);
            return;
        }
        self.role_pending = ctx.inputs.any_pending() || role_pending;
        self.acceleration = match felt_field(ctx, "Liquid") {
            Ok(field) => field,
            Err(error) => {
                ctx.error(error);
                None
            }
        };
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
        let computed = if self.role_pending {
            Ok(None)
        } else {
            self.compute(ctx, &roles, fill)
        };
        self.role_ports.put_back(roles);
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
                let bytes = MAX_LIQUID_BODIES * REACTION_FLOATS * 4;
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
            self.body_buffers.upload(
                gpu,
                &self.bodies,
                fresh && self.rows_fresh,
                "node.gpu_flip_domain.bodies",
            );
            self.region_site_buffer.upload(
                gpu,
                &mut self.body_buffers,
                &self.region_sites,
                fresh && self.rows_fresh,
                "node.gpu_flip_domain.region_sites",
            );
        }
        let uploaded = match ctx
            .gpu
            .as_deref_mut()
            .map(|gpu| self.fields.upload(gpu, ctx.sim_step.offline()))
        {
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
}

impl SharesSolids for GpuFlipDomain {
    fn rigid_owner(&self) -> Option<&LiquidRigidOwner> {
        self.coupled.owner.as_ref()
    }

    fn shared_solids_mut(&mut self) -> &mut SharedSolids {
        &mut self.observed
    }
}

impl PhysicsNode for GpuFlipDomain {
    fn fluid_domain_snapshot(&self) -> Option<manifold_core::fluid_domain::FluidDomainSnapshot> {
        let epoch = self.published.map_or(0, |values| values[EPOCH] as u64);
        Some(manifold_water_liquid::gizmos::domain_snapshot(
            self.layout,
            self.coupled.failed,
            self.role_pending,
            epoch,
            &self.setup_report,
            self.coupled
                .owner
                .as_ref()
                .map_or_else(|| self.observed.uncoupled(), LiquidRigidOwner::uncoupled),
        ))
    }

    /// The pool under the fill height and the initial column as the fill
    /// seeds them (whole half-cell sites), then bodies, regions and forces.
    fn write_physics_gizmos(&self, out: &mut Vec<PhysicsGizmo>) {
        let (Some(layout), Some(_)) = (self.layout, self.setup) else {
            return;
        };
        let (pool_sites, box_sites) = self.fill_sites;
        let site = (layout.cell_size * 0.5) as f32;
        let max: [f32; 3] = std::array::from_fn(|i| layout.min[i] + layout.size[i]);
        if pool_sites > 0 {
            let top = layout.min[1] + pool_sites as f32 * site;
            out.push(PhysicsGizmo::aabb(
                PhysicsGizmoFamily::Fill,
                layout.min,
                [max[0], top, max[2]],
            ));
        }
        if box_sites.iter().all(|[lo, hi]| hi > lo) {
            let corner = |end: usize| {
                std::array::from_fn(|i| layout.min[i] + box_sites[i][end] as f32 * site)
            };
            out.push(PhysicsGizmo::aabb(
                PhysicsGizmoFamily::Fill,
                corner(0),
                corner(1),
            ));
        }
        manifold_water_liquid::gizmos::write_liquid_gizmos(
            &layout,
            &self.bodies,
            self.acceleration.as_ref(),
            out,
        );
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
        authored_sample_only: bool,
    ) {
        // A replay sample hands the scene over before this domain observes
        // the sample's Speed; `run` records it once Speed names the tick. A
        // pending one records nothing.
        if authored_sample_only {
            let scene = observation
                .filter(|_| error.is_none())
                .map(|observation| &observation.inputs);
            self.coupled.scenes.stage(scene);
            return;
        }
        self.set_coupled_physics(true);
        // Refill the retained inputs in place: a steady frame never reallocates.
        match (observation, &mut self.coupled.observation) {
            (Some(source), Some(retained)) => retained.clone_from(source),
            (source, retained) => *retained = source.cloned(),
        }
        self.coupled.colliders = colliders;
        self.coupled.error = error.map(str::to_owned);
    }

    fn coupled_rigid_frame(&self) -> Option<&CoupledRigidFrame> {
        let coupled = &self.coupled;
        if !coupled.mode
            || self.role_pending
            || coupled.failed
            || coupled.observation.is_none()
            || coupled.error.is_some()
        {
            return None;
        }
        coupled.owner.as_ref().map(LiquidRigidOwner::frame)
    }

    fn physics_impulse_epoch(&self) -> Option<u64> {
        self.impulses.epoch()
    }

    /// A pending liquid (roles or field still preparing, clock not started)
    /// makes a hit wait; a failed one refuses it.
    fn physics_impulse_readiness(&self) -> Result<bool, String> {
        if let Some(error) = self.impulses.failure() {
            return Err(error.into());
        }
        if self.coupled.failed {
            return Err("GPU FLIP impulses: the liquid failed; this hit is refused".into());
        }
        Ok(!self.holding && self.impulses.epoch().is_some())
    }

    /// A hit fired this frame is stamped at the liquid's simulated time, so
    /// it lands on the first tick of the next frame.
    fn physics_impulse_stamp(
        &self,
        played: manifold_core::PlayedTime,
        sequence: u64,
        step: manifold_water_rigid::physics::SimStep,
    ) -> Result<manifold_physics::input::EventStamp, String> {
        if !self.physics_impulse_readiness()? {
            return Err(
                "GPU FLIP impulses: the liquid is still preparing; fire waits for it".into(),
            );
        }
        let mut stamp = self.impulses.stamp(played, sequence)?;
        stamp.time = manifold_core::Seconds(
            self.clock
                .simulation_at(played, manifold_water_rigid::physics::live_load(step)),
        );
        Ok(stamp)
    }

    fn enqueue_physics_impulse(
        &mut self,
        stamp: manifold_physics::input::EventStamp,
        impulse: ResolvedNodeImpulse,
    ) -> Result<manifold_physics::TickStamp, String> {
        self.impulses
            .enqueue(stamp, impulse, self.coupled.owner.as_mut())
    }

    fn drain_physics_impulses(
        &mut self,
        consume: &mut dyn FnMut(manifold_physics::input::AppliedEvent<ResolvedNodeImpulse>),
    ) {
        self.impulses
            .drain_applied(consume, self.coupled.owner.as_mut());
    }

    fn drain_discarded_impulses(
        &mut self,
        consume: &mut dyn FnMut(manifold_physics::input::EventStamp),
    ) -> u64 {
        self.impulses
            .drain_discarded(consume, self.coupled.owner.as_mut())
    }
}

inventory::submit! { PhysicsNodeRegistration::new::<GpuFlipDomain>() }

impl TickSampled for GpuFlipDomain {
    fn request_physics_samples(
        &mut self,
        from: manifold_core::PlayedTime,
        until: manifold_core::PlayedTime,
        out: &mut Vec<manifold_core::PlayedTime>,
    ) {
        self.fields.request_samples(&self.clock, from, until, out);
        self.bodies.request_samples(&self.clock, from, until, out);
        if self.coupled.mode {
            self.coupled.scenes.request(&self.clock, from, until, out);
        }
    }
}

inventory::submit! { TickSampledRegistration::new::<GpuFlipDomain>() }

impl GpuFlipDomain {
    /// This frame's outputs, or None while a collider's distance lattice is
    /// still building (the liquid holds and the clock does not advance).
    fn compute(
        &mut self,
        ctx: &EffectNodeContext<'_, '_>,
        roles: &[Option<FluidRole>],
        fill: Result<Option<Transform>, String>,
    ) -> Result<Option<[f32; OUTPUTS.len()]>, String> {
        let geometry = fill
            .map_err(|reason| format!("GPU FLIP: {reason}"))
            .and_then(|column| {
                gpu_flip_geometry(
                    geometry_read(ctx.params, |name, default| {
                        ctx.scalar_or_param(name, default)
                    }),
                    ctx.inputs.transform("domain"),
                    column,
                )
                .map(|geometry| (geometry, column))
            });
        let (geometry, column) = match geometry {
            Ok((geometry, column)) => (geometry.sized_for(role_kinds(roles)), column),
            Err(reason) => {
                self.setup_report.refuse(&reason);
                return Err(reason);
            }
        };
        self.setup_report.accept(
            geometry.fit,
            column.map(|column| (column.pos, column.scale)),
        );
        let speed = ctx.scalar_or_param("speed", 1.0);
        let gravity = [
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
        ];
        if !speed.is_finite()
            || !(0.0..=4.0).contains(&speed)
            || gravity.iter().any(|g| !g.is_finite())
        {
            return Err(
                "GPU FLIP: Speed must be between 0 and 4, and gravity must be finite".into(),
            );
        }
        let drawn_radius = ctx.scalar_or_param(
            "drawn_radius",
            PARTICLE_RADIUS_CELLS * geometry.layout.cell_size as f32,
        );
        if !(geometry.setup.bounds || drawn_radius.is_finite() && drawn_radius >= 0.0) {
            return Err(format!(
                "GPU FLIP: the drawn radius must be finite and not negative, got {drawn_radius}"
            ));
        }
        let offline = ctx.sim_step.offline();
        if let Some(error) = self.coupled.host_error.take() {
            return Err(error);
        }
        let clock = if self.coupled.mode {
            ctx.gpu.as_deref().and_then(|gpu| gpu.device.frame_clock())
        } else {
            None
        };
        let (wall_faces, open_faces) = face_masks(geometry.setup.bounds, closed_faces(ctx.params));
        self.coupled.walls = geometry.domain_walls(wall_faces);
        self.coupled.capacity = MAX_LIQUID_BODIES;
        if self.coupled.mode && !self.observe_rigid(ctx.played(), speed)? {
            return Ok(None);
        }
        let coupled_geometries = self
            .coupled
            .owner
            .as_ref()
            .map_or_else(|| self.observed.geometries(), LiquidRigidOwner::geometries);
        let coupled_rows = self
            .coupled
            .owner
            .as_ref()
            .map_or_else(|| self.observed.rows(), LiquidRigidOwner::rows);
        if self.bodies.prepare(
            roles,
            coupled_geometries,
            coupled_rows,
            self.coupled.walls.bounds(),
            geometry.lattice.cell_size(),
            offline,
        )? == BodiesStatus::Pending
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
                |stamp| {
                    clock.as_ref().is_none_or(|clock| {
                        clock.is_complete(stamp) || (clock.wait(stamp) && clock.is_complete(stamp))
                    })
                },
                |_, rows, impulses| {
                    let offset =
                        offset.ok_or("GPU FLIP coupling: the pending tick has no body offset")?;
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
        // Bodies that joined couple between ticks, never by a restart.
        if !restart
            && let Some(owner) = self.coupled.owner.as_mut()
            && owner.pending().is_none()
            && owner.wants_rebind()
        {
            owner.rebind()?;
            if self.bodies.prepare(
                roles,
                owner.geometries(),
                owner.rows(),
                self.coupled.walls.bounds(),
                geometry.lattice.cell_size(),
                offline,
            )? == BodiesStatus::Pending
            {
                return Ok(None);
            }
        }
        self.setup = Some(geometry.setup);
        self.layout = Some(geometry.layout);
        self.fill_sites = (geometry.pool_sites, geometry.box_sites);
        let frame = self.clock.advance(
            ctx.played(),
            ctx.sim_step.interval.0,
            speed,
            ctx.scalar_or_param("reset", 0.0),
            restart,
            offline,
        );
        self.scheduled_frame = Some(frame.clone());
        if frame.numerical_error {
            ctx.sim_metrics.record_simulation(0.0, 0.0, false, true);
        }
        if frame.restarted && self.coupled.owner.is_some() && !self.coupled.owner_fresh {
            self.rebuild_owner()?;
        }
        self.coupled.owner_fresh = false;
        if let Some(owner) = self.coupled.owner.as_mut() {
            ctx.sim_metrics.merge(&owner.rigid_mut().take_metrics());
        }
        // After reconciliation, so a restart never reports the old owner's completion.
        let completed = self.coupled.owner.as_ref().map(LiquidRigidOwner::completed);
        ctx.sim_metrics.record(|metrics| {
            manifold_water_rigid::physics_metrics::record_clock(
                metrics,
                &self.clock,
                &frame,
                completed,
            )
        });
        if let Some(owner) = &self.coupled.owner {
            let scene = self
                .coupled
                .observation
                .as_ref()
                .map(|observation| &observation.inputs);
            self.coupled.scenes.settle(&self.clock, &frame, scene);
            self.coupled.scenes.prune_before(owner.completed());
        }
        self.impulses.observe_frame(ctx.played(), &frame)?;
        self.bodies.settle(roles, &self.clock, &frame);
        let first_tick = fields::first_tick(&frame);
        // A restart runs no tick yet still publishes the first tick's rows: the
        // fill seeds around the bodies' starting poses. Otherwise a frame
        // without ticks keeps the last rows as the bodies' poses.
        let row_ticks = if frame.restarted {
            frame.ticks.max(1)
        } else {
            frame.ticks
        };
        self.rows_fresh = row_ticks > 0;
        let coupled_rows = self
            .coupled
            .owner
            .as_ref()
            .map_or_else(|| self.observed.rows(), LiquidRigidOwner::rows);
        if row_ticks > 0 {
            self.body_rows = self
                .bodies
                .rows(first_tick, frame.ticks, coupled_rows)?
                .len() as f32;
            self.bodies.set_contacts(
                self.coupled
                    .owner
                    .as_ref()
                    .map_or_else(|| self.observed.contacts(), LiquidRigidOwner::contacts),
            )?;
            // Each Inflow's site box over the poses its tick can read: the
            // tick's length, and never less than the nominal tick.
            self.region_sites.rebuild(
                self.bodies.last_region_rows(),
                self.bodies.region_count(),
                self.bodies.shapes(),
                geometry.lattice.cell_size(),
                |tick| {
                    frame
                        .interval(tick as u64)
                        .map_or(0.0, |interval| interval.duration().0 as f32)
                        .max(TICK as f32)
                },
            )?;
        }
        let dynamic_bodies = coupled_rows
            .iter()
            .filter(|row| takes_reaction(row))
            .count();
        // Another water's bodies move on through each later tick of this frame.
        if row_ticks > 0 && self.coupled.owner.is_none() && !self.observed.rows().is_empty() {
            let mut seconds = 0.0;
            for tick in 1..frame.ticks as usize {
                seconds += frame
                    .interval(tick as u64 - 1)
                    .map_or(0.0, |interval| interval.duration().0 as f32);
                self.bodies
                    .set_coupled_rows(tick, self.observed.rows_after(seconds))?;
            }
        }
        self.observed.used();
        let (scene, kill_height) = self.scene(&geometry, column, frame.epoch, drawn_radius);
        self.scene_box = scene;
        self.bodies.prepare_clock_vertices(
            scene[0],
            std::array::from_fn(|i| scene[1][i] - scene[0][i]),
            0,
        );
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
                let pending = PendingTick {
                    tick: first_tick,
                    stamp: clock.as_ref().map_or(0, FrameClock::stamp),
                    interval: frame.interval(0).expect("pending interval"),
                    offline: frame.offline,
                };
                let offset = self.bodies.count() - owner.rows().len();
                owner.set_pending(pending);
                self.coupled.offset = Some(offset);
                if frame.ticks > 1 {
                    self.coupled.exchange = Some(Exchange {
                        frame: frame.clone(),
                        ticks: frame.ticks,
                        pending,
                        offset,
                    });
                }
            }
            // The water is shown at the tick Box3D has settled, with the
            // bodies' accepted frame, which the scene takes before the region
            // runs this frame's ticks.
            display_time = owner.completed_time().0;
        }
        self.fields.split_live_hits();
        let field = self.fields.prepare(
            geometry.field_lattice(scene),
            self.acceleration.as_ref(),
            &self.clock,
            &frame,
            &self.impulses,
        )?;
        let handover = self
            .coupled
            .owner
            .as_ref()
            .map_or_else(Default::default, LiquidRigidOwner::handover);
        let per_frame = [
            ("closed_faces", wall_faces as f32),
            ("open_faces", open_faces as f32),
            ("kill_height", kill_height),
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
            (
                "display_cursor",
                if offline || self.coupled.owner.is_some() {
                    0.0
                } else {
                    1.0
                },
            ),
            ("dropped_seconds", frame.dropped_seconds as f32),
            ("body_count", self.bodies.count() as f32),
            ("region_count", self.bodies.region_count() as f32),
            ("region_site_total", self.region_sites.total() as f32),
            ("body_rows", self.body_rows),
            ("dynamic_bodies", dynamic_bodies as f32),
            // How far Box3D ended the last settled tick from the coupled motion
            // law, worst body (m, m/s, rad; D18).
            ("handover_position", handover.position),
            ("handover_velocity", handover.velocity),
            ("handover_rotation", handover.rotation),
            ("first_tick", first_tick as f32),
        ];
        let simulated: f64 = (0..u64::from(frame.ticks))
            .map(|tick| {
                frame
                    .interval(tick)
                    .map_or(0.0, |interval| interval.duration().0)
            })
            .sum();
        let walls = wall_velocity_outputs(
            if restart || frame.restarted {
                None
            } else {
                self.last_bounds
            },
            geometry.bounds_box,
            simulated,
        );
        self.last_bounds = geometry.setup.bounds.then_some(geometry.bounds_box);
        let mut values = [0.0; OUTPUTS.len()];
        let mut written = 0u128;
        for (name, value) in geometry
            .outputs()
            .into_iter()
            .chain(bounds_outputs(scene))
            .chain(walls)
            .chain(per_frame)
            .chain(field.outputs())
        {
            let slot = OUTPUTS
                .iter()
                .position(|output| *output == name)
                .expect("every output has a slot");
            values[slot] = value;
            written |= 1 << slot;
        }
        debug_assert_eq!(
            written,
            (1 << OUTPUTS.len()) - 1,
            "every output written once"
        );
        Ok(Some(values))
    }

    /// This frame's scene box and kill height, from the bodies and regions
    /// of the last rows. With Bounds on the scene box is the Bounds box and
    /// nothing reads the kill height. With Bounds off the scene box is the box
    /// round every body, region and the Initial Fill `column` (the Water
    /// item's own box when none is placed, so the lattices keep an extent),
    /// and the kill height hangs from the bodies ([`kill_height`]) or, with
    /// none, from the lowest source of the epoch `epoch`'s start.
    fn scene(
        &mut self,
        geometry: &GpuFlipGeometry,
        column: Option<Transform>,
        epoch: u32,
        drawn_radius: f32,
    ) -> ([[f32; 3]; 2], f32) {
        if geometry.setup.bounds {
            return (geometry.bounds_box, NO_KILL_HEIGHT);
        }
        if self.source_floor.epoch != Some(epoch) {
            let inflows = self
                .bodies
                .region_boxes()
                .filter(|(kind, _)| *kind == FluidRoleKind::Inflow)
                .map(|(_, placed)| placed[0][1]);
            self.source_floor = SourceFloor {
                epoch: Some(epoch),
                y: lowest_source(column, inflows),
            };
        }
        let kill = kill_height(
            self.bodies
                .body_boxes()
                .map(|(fixed, placed)| (fixed, placed[0][1])),
            self.source_floor.y,
            drawn_radius,
        );
        let placed = self
            .bodies
            .body_boxes()
            .map(|(_, placed)| placed)
            .chain(self.bodies.region_boxes().map(|(_, placed)| placed))
            .chain(column.map(|fill| box_about(fill.pos, fill.scale)));
        match union_box(placed) {
            Some(scene) => (scene, kill),
            None => (geometry.bounds_box, kill),
        }
    }

    /// Take the paired world's observation for this frame: check it shares
    /// the water's clock, and build a new rigid owner (restarting the pair)
    /// on its reset or a change of bodies. False while the world's inputs are
    /// still pending.
    fn observe_rigid(
        &mut self,
        played: manifold_core::PlayedTime,
        speed: f32,
    ) -> Result<bool, String> {
        let Coupling {
            observation,
            colliders,
            error,
            previous_reset,
            owner,
            owner_fresh,
            epochs,
            walls,
            capacity,
            step,
            ..
        } = &mut self.coupled;
        if let Some(error) = error {
            return Err(error.clone());
        }
        let Some(observation) = observation.as_ref() else {
            return Ok(false);
        };
        if speed != observation.speed {
            return Err(format!(
                "GPU FLIP coupling: water and rigid bodies require the same Speed and shared World control (water {speed}, rigid {})",
                observation.speed
            ));
        }
        if (observation.transport.seconds() - played.seconds()).abs() > 1e-9 {
            return Err(
                "GPU FLIP coupling: rigid observation transport does not match water transport"
                    .into(),
            );
        }
        CoupledRigidInputs {
            scene: &observation.inputs,
            colliders,
            density: f64::from(WATER_DENSITY),
        }
        .validate()?;
        let reset_edge = previous_reset.is_some_and(|previous| previous != observation.reset);
        *previous_reset = Some(observation.reset);
        // A body that joined is admitted into the running world; only a
        // change it cannot take live starts a new epoch.
        let changed = owner.as_ref().is_none_or(|owner| {
            !owner.matches(&observation.inputs, *walls, &*colliders, *capacity)
        });
        let admitted = !reset_edge
            && changed
            && match owner.as_mut() {
                Some(owner) => owner.admit(&observation.inputs, *walls, &*colliders, *capacity)?,
                None => false,
            };
        if reset_edge || (changed && !admitted) {
            *epochs += 1;
            let mut built = LiquidRigidOwner::new(
                &observation.inputs,
                *walls,
                &*colliders,
                *capacity,
                *epochs,
                owner.as_ref(),
            )?;
            built.set_step(*step);
            *owner = Some(built);
            *owner_fresh = true;
        }
        Ok(true)
    }

    /// Exchange before tick `tick` of this frame (counted from its first):
    /// the GPU has finished tick `tick − 1`, so its reaction is final and the
    /// body rows are free to rewrite.
    fn exchange_tick(&mut self, tick: u32, exchange: Exchange) -> Result<(), String> {
        let owner = self
            .coupled
            .owner
            .as_mut()
            .ok_or("GPU FLIP coupling: no coupled rigid world")?;
        let reaction = self
            .reaction
            .as_ref()
            .ok_or("GPU FLIP coupling: the reaction is missing")?;
        // A tick running this frame started by now, so the tick before it
        // has its end sampled.
        let end = self.coupled.scenes.endpoint(owner.completed() + 1);
        owner.settle_ready(
            end,
            |_| true,
            |_, rows, impulses| {
                decode_reaction(
                    exchange.offset,
                    rows,
                    reaction_floats(Some(reaction)),
                    impulses,
                )
            },
        )?;
        let (offset, rows) = self.bodies.set_coupled_rows(tick as usize, owner.rows())?;
        let bodies = self
            .body_buffers
            .bodies()
            .ok_or("GPU FLIP coupling: the body rows are missing")?;
        let bytes: &[u8] = bytemuck::cast_slice(rows);
        if offset + bytes.len() as u64 > bodies.size {
            return Err("GPU FLIP coupling: the body rows outgrew their buffer".into());
        }
        // SAFETY: shared storage in bounds (checked above); the GPU has
        // completed every command that touched it, and the next reader is
        // encoded after this write.
        unsafe { bodies.write(offset, bytes) };
        let (offset, contacts) = self
            .bodies
            .set_coupled_contacts(tick as usize, owner.contacts())?;
        let buffer = self
            .body_buffers
            .contacts()
            .ok_or("GPU FLIP coupling: the contact normals are missing")?;
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
        self.bodies.prepare_clock_vertices(
            self.scene_box[0],
            std::array::from_fn(|i| self.scene_box[1][i] - self.scene_box[0][i]),
            tick as usize,
        );
        for (port, vertices) in [
            ("clock_obstacles", self.bodies.clock_obstacles()),
            ("clock_sources", self.bodies.clock_sources()),
        ] {
            let buffer = self
                .body_buffers
                .output(port)
                .ok_or_else(|| format!("GPU FLIP coupling: {port} is missing"))?;
            let bytes = bytemuck::cast_slice(vertices);
            if bytes.len() as u64 > buffer.size {
                return Err(format!("GPU FLIP coupling: {port} outgrew its buffer"));
            }
            // SAFETY: the same completed fence and bounds check as body rows.
            unsafe { buffer.write(0, bytes) };
        }
        reaction.zero_fill();
        owner.set_pending(PendingTick {
            tick: exchange.pending.tick + u64::from(tick),
            interval: exchange
                .frame
                .interval(u64::from(tick))
                .expect("accepted exchange interval"),
            ..exchange.pending
        });
        Ok(())
    }

    /// A restart the water's own clock found (its reset, a backward seek):
    /// the rigid world restarts with it, keeping its hulls.
    fn rebuild_owner(&mut self) -> Result<(), String> {
        let observation = self
            .coupled
            .observation
            .as_ref()
            .ok_or("GPU FLIP coupling: the rigid observation is missing")?;
        self.coupled.epochs += 1;
        let mut owner = LiquidRigidOwner::new(
            &observation.inputs,
            self.coupled.walls,
            &self.coupled.colliders,
            self.coupled.capacity,
            self.coupled.epochs,
            self.coupled.owner.as_ref(),
        )?;
        owner.set_step(self.coupled.step);
        self.coupled.owner = Some(owner);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PlayedTime;
    use manifold_water_liquid::bodies::posed_box;

    fn geometry(
        resolution: f32,
        fill: f32,
        volume: Option<Transform>,
    ) -> Result<GpuFlipGeometry, String> {
        let read = |name: &str, default: f32| match name {
            "cell_metres" => 4.0 / resolution,
            "fill_height" => fill,
            _ => default,
        };
        gpu_flip_geometry(read, None, volume)
    }

    /// A pending liquid makes a hit wait for it; only a failed one refuses
    /// (BUG-mo4oa (scene force trigger skipped)).
    #[test]
    fn gpu_flip_pending_liquid_makes_hits_wait() {
        use manifold_physics::clock::{SimulationClock, TICK};
        let mut domain = GpuFlipDomain::new();
        assert_eq!(
            domain.physics_impulse_readiness(),
            Ok(false),
            "the clock has not started"
        );
        let frame = SimulationClock::default().advance(
            PlayedTime::from_played(0.0),
            TICK,
            1.0,
            0.0,
            false,
            false,
        );
        domain
            .impulses
            .observe_frame(PlayedTime::from_played(0.0), &frame)
            .unwrap();
        assert_eq!(domain.physics_impulse_readiness(), Ok(true));
        domain.holding = true;
        assert_eq!(
            domain.physics_impulse_readiness(),
            Ok(false),
            "a role or the field is still preparing"
        );
        domain.coupled.failed = true;
        assert!(
            domain
                .physics_impulse_readiness()
                .unwrap_err()
                .contains("refused")
        );
    }

    /// The field lattice lies on the scene box it is given from its minimum
    /// corner to the far corner, with the node count along the longest side
    /// the lattice over the face grid has, and fewer nodes on a short side.
    #[test]
    fn gpu_flip_domain_fields_cover_the_scene_box() {
        let geometry = geometry(64.0, 0.16, None).unwrap();
        let grid = FlipSolverGrid::from_lattice(geometry.lattice);
        let covering =
            ForceLattice::covering(grid.min(), geometry.lattice.cell_size(), grid.nodes());
        let detail = covering.nodes().into_iter().max().unwrap();
        for scene in [
            geometry.bounds_box,
            [[-3.1, 0.2, 5.0], [4.9, 1.2, 7.5]],
            [[-40.0, -9.0, 2.0], [-39.0, -8.5, 3.5]],
        ] {
            let field = geometry.field_lattice(scene);
            let size: [f32; 3] = std::array::from_fn(|i| scene[1][i] - scene[0][i]);
            let longest = size.into_iter().fold(0.0, f32::max);
            assert_eq!(field.min(), scene[0]);
            assert_eq!(field.nodes().into_iter().max(), Some(detail));
            for (axis, &extent) in size.iter().enumerate() {
                let reach = (field.nodes()[axis] - 1) as f32 * field.spacing();
                assert!(
                    reach >= extent - 1e-4 && reach < extent + field.spacing() + 1e-4,
                    "axis {axis}: {reach} for {extent}"
                );
                if extent < 0.5 * longest {
                    assert!(field.nodes()[axis] < detail, "axis {axis} is short");
                }
            }
        }
        // The Dam Break's box at 4 m is the lattice the face grid always had.
        assert_eq!(
            geometry.field_lattice(geometry.bounds_box).nodes(),
            covering.nodes()
        );
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
                outputs
                    .iter()
                    .find(|(port, _)| *port == name)
                    .map_or(default, |(_, value)| *value)
            })
            .unwrap();
            assert_eq!(lattice, setup.lattice);
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
            let at = geometry(resolution, 0.16, None)
                .unwrap_or_else(|reason| panic!("{resolution}: {reason}"));
            let n = resolution as u32;
            assert_eq!(at.lattice.cells(), [n; 3]);
            assert_eq!(at.lattice.nodes(), [n + 7; 3]);
        }
    }

    /// The Dam Break's column, as the preset wires it: the engine's boxes on
    /// the engine's half-cell site rule.
    #[test]
    fn gpu_flip_domain_fills_the_dam_break_sites() {
        let column = Transform {
            pos: [-1.25, 1.12, 0.0],
            scale: [1.18, 1.92, 3.5],
            ..Transform::default()
        };
        let at64 = geometry(64.0, 0.16, Some(column)).expect("64 fills");
        assert_eq!(
            (at64.pool_sites, at64.box_sites),
            (5, [[5, 43], [5, 67], [8, 120]])
        );
        assert_eq!(at64.particles, 128 * 5 * 128 + 38 * 62 * 112);
        assert_eq!(at64.lattice.nodes(), [71; 3]);
        let mass = at64
            .outputs()
            .iter()
            .find(|(name, _)| *name == "particle_mass")
            .expect("mass")
            .1;
        assert!((mass - 0.030_517_578).abs() < 1e-9, "{mass}");
    }

    /// The authored box is moved onto whole world cells (cell p centred on
    /// p·h) by less than half a cell, its size and cell count kept, and the
    /// lattice, the solver grid's first cell and the mesh grid follow it. The
    /// Dam Break's box is already on them and does not move.
    #[test]
    fn gpu_flip_domain_puts_the_box_on_whole_world_cells() {
        use manifold_water_liquid::primitives::face_sample_component::solver_origin_cell;
        let output = |g: &GpuFlipGeometry, name: &str| {
            g.outputs()
                .into_iter()
                .find(|(port, _)| *port == name)
                .unwrap_or_else(|| panic!("{name}"))
                .1
        };
        let dam_break = geometry(64.0, 0.16, None).unwrap();
        assert_eq!(dam_break.layout, domain_layout(None, 4.0, 0.0625).unwrap());
        assert_eq!(dam_break.cell_origin(), [-33, -1, -33]);

        // Off the grid on every axis, across the world origin on x and z: the
        // box minimum -1.63, -0.9, -2.8 is cells -26.08, -14.4 and -44.8.
        let pose = Transform {
            pos: [0.37, 1.1, -0.8],
            scale: [4.0; 3],
            ..Transform::default()
        };
        let read = |name: &str, default: f32| {
            if name == "cell_metres" {
                0.0625
            } else {
                default
            }
        };
        let moved = gpu_flip_geometry(read, Some(pose), None).expect("fills");
        let h = moved.layout.cell_size;
        assert_eq!(h, 0.0625);
        assert_eq!(moved.layout.min, [-1.625, -0.875, -2.8125]);
        assert_eq!(moved.layout.size, [4.0; 3]);
        assert_eq!(moved.layout.cells, [64; 3]);
        // The Bounds box is where the Water item sits, not moved onto cells.
        assert_eq!(
            moved.bounds_box,
            [
                [0.37 - 2.0, 1.1 - 2.0, -0.8 - 2.0],
                [0.37 + 2.0, 1.1 + 2.0, -0.8 + 2.0]
            ]
        );
        assert_eq!(moved.cell_origin(), [-27, -15, -46]);
        // The padded lattice starts 3 cells below the box; the mesh grid
        // 1.5 cells; both on the same world cells.
        assert_eq!(output(&moved, "lattice_min_x"), -1.8125);
        assert_eq!(output(&moved, "mesh_min_y"), -0.875 - 1.5 * 0.0625);
        for (axis, port) in ["cell_origin_x", "cell_origin_y", "cell_origin_z"]
            .into_iter()
            .enumerate()
        {
            assert_eq!(output(&moved, port), moved.cell_origin()[axis] as f32);
        }
        let mesh_min = FlipSolverGrid::from_lattice(moved.lattice).min();
        assert_eq!(solver_origin_cell(mesh_min, h as f32), moved.cell_origin());
        assert!(
            moved
                .outputs()
                .iter()
                .all(|(name, _)| OUTPUTS.contains(name))
        );

        // An odd cell count puts the box edge half a cell off the grid; it
        // moves the full half cell, not more.
        let odd = geometry(63.0, 0.16, None).unwrap();
        let h = odd.layout.cell_size;
        let unmoved = domain_layout(None, 4.0, f64::from(4.0_f32 / 63.0)).unwrap();
        for axis in [0, 2] {
            let shift = (f64::from(odd.layout.min[axis]) - f64::from(unmoved.min[axis])).abs();
            assert!((shift - 0.5 * h).abs() < 1e-6, "axis {axis}: {shift}");
            let cell = f64::from(odd.layout.min[axis]) / h;
            assert!((cell - cell.round()).abs() < 1e-5, "axis {axis}: {cell}");
        }
    }

    /// The fill's sites are world half cells: site j of a box starting at
    /// world cell m is half cell 2m + 1 + j, at (J/2 - 1/4)·h, whatever the
    /// domain's pose, and the column's sites are the ones inside its edges.
    #[test]
    fn gpu_flip_domain_fill_sites_are_world_half_cells() {
        let pose = Transform {
            pos: [0.37, 1.1, -0.8],
            scale: [4.0; 3],
            ..Transform::default()
        };
        let column = Transform {
            pos: [0.1, 0.9, -0.5],
            scale: [0.61, 0.43, 0.77],
            ..Transform::default()
        };
        let read = |name: &str, default: f32| match name {
            "cell_metres" => 0.0625,
            "fill_height" => 0.16,
            _ => default,
        };
        let at = gpu_flip_geometry(read, Some(pose), Some(column)).expect("fills");
        let h = at.layout.cell_size;
        let box_cell = at.cell_origin().map(|c| c + 1);
        for (axis, &cell) in box_cell.iter().enumerate() {
            let world = |site: u32| {
                let half = 2 * i64::from(cell) + 1 + i64::from(site);
                (0.5 * half as f64 - 0.25) * h
            };
            // The box-relative position the fill kernel computes.
            let local =
                |site: u32| f64::from(at.layout.min[axis]) + (0.25 + 0.5 * f64::from(site)) * h;
            let [first, end] = at.box_sites[axis];
            assert!(end > first);
            assert!((world(first) - local(first)).abs() < 1e-5, "axis {axis}");
            let lo = f64::from(column.pos[axis] - 0.5 * column.scale[axis]);
            let hi = f64::from(column.pos[axis] + 0.5 * column.scale[axis]);
            assert!(
                world(first) >= lo - 1e-6 && world(first - 1) < lo,
                "axis {axis}"
            );
            assert!(
                world(end - 1) < hi && world(end) >= hi - 1e-6,
                "axis {axis}"
            );
        }
    }

    /// The Dam Break's fill gizmos are the sites the fill seeds: the pool
    /// up to its five site layers and the column within a site of the
    /// authored box. The accepted domain reports Ready with its layout.
    #[test]
    fn gpu_flip_dam_break_fill_gizmos_are_the_seeded_sites() {
        use manifold_core::physics_gizmo::PhysicsGizmoShape;
        let column = Transform {
            pos: [-1.25, 1.12, 0.0],
            scale: [1.18, 1.92, 3.5],
            ..Transform::default()
        };
        let at64 = geometry(64.0, 0.16, Some(column)).expect("64 fills");
        let mut domain = GpuFlipDomain::new();
        let mut out = Vec::new();
        domain.write_physics_gizmos(&mut out);
        assert!(out.is_empty(), "nothing before a setup is accepted");
        assert!(
            domain
                .fluid_domain_snapshot()
                .unwrap()
                .accepted_layout
                .is_none()
        );
        (domain.setup, domain.layout) = (Some(at64.setup), Some(at64.layout));
        domain.fill_sites = (at64.pool_sites, at64.box_sites);
        let snapshot = domain.fluid_domain_snapshot().unwrap();
        assert_eq!(
            (snapshot.state, snapshot.accepted_layout),
            (
                manifold_core::fluid_domain::FluidDomainState::Ready,
                Some(at64.layout)
            )
        );
        domain.write_physics_gizmos(&mut out);
        assert!(
            out.iter()
                .all(|gizmo| gizmo.family == PhysicsGizmoFamily::Fill)
        );
        let boxes: Vec<_> = out
            .iter()
            .map(|gizmo| match gizmo.shape {
                PhysicsGizmoShape::Box { center, half, .. } => (center, half),
                other => panic!("{other:?}"),
            })
            .collect();
        let layout = at64.layout;
        let site = (layout.cell_size * 0.5) as f32;
        let pool_top = boxes[0].0[1] + boxes[0].1[1];
        assert!(
            (pool_top - (layout.min[1] + 5.0 * site)).abs() < 1e-6
                && (boxes[0].1[0] - layout.size[0] * 0.5).abs() < 1e-6
        );
        for axis in 0..3 {
            assert!(
                (boxes[1].0[axis] - column.pos[axis]).abs() <= site,
                "axis {axis}: {:?}",
                boxes[1]
            );
            assert!(
                (2.0 * boxes[1].1[axis] - column.scale[axis]).abs() <= 2.0 * site,
                "axis {axis}: {:?}",
                boxes[1]
            );
        }
        assert_eq!(boxes.len(), 2, "no bodies or forces in a bare domain");
    }

    /// Moving or resizing the Bounds box leaves the setup alone, so the water
    /// keeps running; the cell size, the Bounds switch and Fill Height are
    /// what restart it.
    #[test]
    fn gpu_flip_moving_or_resizing_the_box_keeps_the_setup() {
        let at = |pose: Transform, cell: f32, bounds: f32, fill: f32| {
            let read = |name: &str, default: f32| match name {
                "cell_metres" => cell,
                "bounds" => bounds,
                "fill_height" => fill,
                _ => default,
            };
            gpu_flip_geometry(read, Some(pose), None).expect("fits")
        };
        let pose = Transform {
            pos: [0.37, 1.1, -0.8],
            scale: [4.0; 3],
            ..Transform::default()
        };
        let base = at(pose, 0.0625, 1.0, 0.16);
        let moved = at(
            Transform {
                pos: [0.67, 1.1, -0.8],
                ..pose
            },
            0.0625,
            1.0,
            0.16,
        );
        let grown = at(
            Transform {
                scale: [4.5; 3],
                ..pose
            },
            0.0625,
            1.0,
            0.16,
        );
        assert_eq!(moved.setup, base.setup);
        assert_eq!(grown.setup, base.setup);
        assert_ne!(moved.bounds_box, base.bounds_box);
        assert_ne!(grown.bounds_box, base.bounds_box);
        assert_ne!(at(pose, 0.125, 1.0, 0.16).setup, base.setup);
        assert_ne!(at(pose, 0.0625, 0.0, 0.0).setup, base.setup);
        assert_ne!(at(pose, 0.0625, 1.0, 0.32).setup, base.setup);
    }

    /// Each face's velocity is the box edge's travel over the simulated time;
    /// nothing before a previous box, with no time run, or on a restart.
    #[test]
    fn gpu_flip_wall_velocity_is_the_edge_travel_over_simulated_time() {
        let before = [[-2.0, 0.0, -2.0], [2.0, 4.0, 2.0]];
        let now = [[-1.7, 0.0, -2.0], [2.5, 4.0, 2.0]];
        let value =
            |out: [(&str, f32); 6], name: &str| out.iter().find(|(n, _)| *n == name).unwrap().1;
        let out = wall_velocity_outputs(Some(before), now, 0.1);
        assert!((value(out, "wall_velocity_lo_x") - 3.0).abs() < 1e-5);
        assert!((value(out, "wall_velocity_hi_x") - 5.0).abs() < 1e-5);
        assert_eq!(value(out, "wall_velocity_lo_y"), 0.0);
        for out in [
            wall_velocity_outputs(None, now, 0.1),
            wall_velocity_outputs(Some(before), now, 0.0),
        ] {
            assert!(out.iter().all(|(_, v)| *v == 0.0));
        }
        assert!(OUTPUTS.contains(&"wall_velocity_hi_z"));
    }

    #[test]
    fn gpu_flip_domain_refuses_by_name() {
        let refused = |result: Result<GpuFlipGeometry, String>| result.expect_err("refused");
        assert!(refused(geometry(64.0, -1.0, None)).contains("Fill Height"));
        // With Bounds off there is no floor to fill from; 0 is the way to say so.
        let bounds_off = |fill: f32| {
            gpu_flip_geometry(
                |name, default| match name {
                    "bounds" => 0.0,
                    "fill_height" => fill,
                    _ => default,
                },
                None,
                None,
            )
        };
        let reason = refused(bounds_off(0.16));
        assert!(
            reason.contains("Fill Height") && reason.contains("Bounds off"),
            "{reason}"
        );
        assert_eq!(bounds_off(0.0).expect("no fill height").particles, 0);
        let turned = Transform {
            pos: [0.0, 1.0, 0.0],
            scale: [1.0; 3],
            rot_euler: [0.0, 0.3, 0.0],
            ..Transform::default()
        };
        assert!(refused(geometry(64.0, 0.16, Some(turned))).contains("Initial Fill"));
        // 256³ with a 2.5 m pool is 8 · 256² · 160 particles, past 2^24.
        let over = refused(geometry(256.0, 2.5, None));
        assert!(
            over.contains("Cell Size") && over.contains("Fill Height"),
            "{over}"
        );
    }

    /// A fill that does not fit the domain gets the shared fit's defined
    /// result, the one the setup check reports: the pool caps at the top, a
    /// column past a wall fills what is inside, one wholly outside fills
    /// nothing (`manifold_core::liquid_setup`).
    #[test]
    fn gpu_flip_domain_caps_the_fill_and_clips_the_column() {
        let top = geometry(64.0, 4.0, None).expect("a pool to the top fills");
        let past = geometry(64.0, 9.0, None).expect("a pool past the top is capped");
        assert_eq!(past.pool_sites, top.pool_sites);
        assert_eq!(past.particles, top.particles);

        // The default 4 m domain spans x from -2 to 2.
        let at_wall = Transform {
            pos: [1.5, 1.0, 0.0],
            scale: [1.0; 3],
            ..Transform::default()
        };
        let past_wall = Transform {
            pos: [2.0, 1.0, 0.0],
            scale: [2.0, 1.0, 1.0],
            ..Transform::default()
        };
        let inside = geometry(64.0, 0.16, Some(at_wall)).expect("fills");
        let clipped = geometry(64.0, 0.16, Some(past_wall)).expect("a clipped column fills");
        assert_eq!(
            clipped.box_sites, inside.box_sites,
            "the part inside is the same fill"
        );
        assert!(clipped.particles > geometry(64.0, 0.16, None).unwrap().particles);

        let away = Transform {
            pos: [9.0, 1.0, 0.0],
            scale: [1.0; 3],
            ..Transform::default()
        };
        let none = geometry(64.0, 0.16, Some(away)).expect("a column outside fills nothing");
        assert_eq!(none.box_sites, [[0; 2]; 3]);
        assert_eq!(
            none.particles,
            geometry(64.0, 0.16, None).unwrap().particles
        );
    }

    /// A column that fits sites exactly as before the shared fit existed:
    /// the Sea Wall domain (8 m, resolution 112) with a column at x = -2.83,
    /// 0.41 wide, sites x from 27 to 39. Building the edges in f32 moved the
    /// upper edge one layer.
    #[test]
    fn gpu_flip_domain_sites_a_fitting_column_as_before() {
        let read = |name: &str, default: f32| match name {
            "domain_size" => 8.0,
            "cell_metres" => 8.0 / 112.0,
            "fill_height" => 0.16,
            _ => default,
        };
        let column = Transform {
            pos: [-2.83, 1.0, 0.0],
            scale: [0.41, 1.0, 1.0],
            ..Transform::default()
        };
        let at = gpu_flip_geometry(read, None, Some(column)).expect("fills");
        assert_eq!(at.box_sites[0], [27, 39]);
    }

    /// BUG-3zq4r (inflow runs out): the pool used to be the fill's count, so
    /// an inflow only had the sites the fill left dead inside solids and
    /// stopped within seconds, silently. With an Inflow the pool holds every
    /// half-cell site of the domain up to about 1 GB of slots: an inflow
    /// emits only at empty sites, so it is never short of a slot while any
    /// site is empty, and runs as long as the clip does. A domain without an
    /// inflow keeps the fill's count, and an old show's Max Particles changes
    /// nothing.
    #[test]
    fn gpu_flip_domain_makes_room_for_an_inflow() {
        let at_resolution = |resolution: f32, saved_capacity: f32| {
            let read = |name: &str, default: f32| match name {
                "cell_metres" => 4.0 / resolution,
                "fill_height" => 0.16,
                "particle_capacity" => saved_capacity,
                _ => default,
            };
            gpu_flip_geometry(read, None, None).expect("fills")
        };
        let at = |saved_capacity: f32| at_resolution(32.0, saved_capacity);
        let still = at(0.0);
        assert_eq!(
            u64::from(still.setup.particle_capacity),
            0,
            "no inflow: the fill's count"
        );
        let flowing = at(0.0).with_inflow();
        let sites: u64 = still
            .layout
            .cells
            .iter()
            .map(|&n| 2 * u64::from(n))
            .product();
        assert_eq!(sites, 64 * 64 * 64, "the 4 m cube at 32 cells a side");
        assert_eq!(u64::from(flowing.setup.particle_capacity), sites);
        assert_eq!(flowing.particles, sites);
        assert!(
            flowing.particles > still.particles * 10,
            "room for the domain, not the 0.16 m pool"
        );
        assert_ne!(
            flowing.setup, still.setup,
            "adding an inflow restarts the water at the new size"
        );
        let output = |g: &GpuFlipGeometry| {
            g.outputs()
                .into_iter()
                .find(|(name, _)| *name == "particle_capacity")
                .map(|(_, v)| v)
        };
        assert_eq!(
            output(&flowing),
            Some(sites as f32),
            "the fill's slots follow the wire"
        );
        assert_eq!(at(5000.0), still, "a saved Max Particles is not read");
        assert_eq!(at(5000.0).with_inflow(), flowing, "nor with an inflow");
        // 64 cells a side hold 2 097 152 sites: the budget, about 1 GB.
        for resolution in [64.0, 128.0, 256.0] {
            let big = at_resolution(resolution, 0.0).with_inflow();
            assert_eq!(
                big.setup.particle_capacity, AUTO_INFLOW_SLOTS,
                "resolution {resolution}"
            );
        }
    }

    /// Bounds off has neither mask, no slab and no box to crop bodies to, and
    /// switching it restarts the water; Bounds on with two Open faces (Floor
    /// and Front) has the Wall and Open masks complement each other, a slab
    /// per Wall face at the box as placed, and the same crop with every face
    /// Open.
    #[test]
    fn gpu_flip_domain_bounds_decide_the_masks_and_the_walls() {
        let pose = Transform {
            pos: [0.37, 1.1, -0.8],
            scale: [4.0; 3],
            ..Transform::default()
        };
        let at = |bounds: f32| {
            let read = |name: &str, default: f32| match name {
                "bounds" => bounds,
                "fill_height" => 0.0,
                _ => default,
            };
            gpu_flip_geometry(read, Some(pose), None).expect("fits")
        };
        let (off, on) = (at(0.0), at(1.0));
        assert!(!off.setup.bounds && on.setup.bounds);
        assert_ne!(on.setup, off.setup, "Bounds on or off restarts the water");
        assert_eq!(
            on.bounds_box, off.bounds_box,
            "the box is the same either way"
        );

        let (walls, open) = face_masks(off.setup.bounds, ALL_FACES);
        assert_eq!((walls, open), (0, 0));
        let none = off.domain_walls(walls);
        assert_eq!(none.slabs().count(), 0);
        assert_eq!(none.bounds(), None, "no box to crop the bodies to");

        let closed = ALL_FACES & !(1 << 2) & !(1 << 5);
        let (walls, open) = face_masks(on.setup.bounds, closed);
        assert_eq!((walls, open), (0b01_1011, 0b10_0100));
        assert_eq!((walls | open, walls & open), (ALL_FACES, 0));
        let tank = on.domain_walls(walls);
        assert_eq!(tank.slabs().count(), 4);
        assert_eq!(tank.bounds(), Some(on.bounds_box));

        // The Left wall's slab sits just outside the box as placed, a
        // twentieth of the longest side thick, centred on the other axes.
        let left = on.domain_walls(1);
        let slab = left.slabs().next().expect("one Wall face");
        let [lo, hi] = on.bounds_box;
        let expected = [
            lo[0] - 0.05 * 4.0,
            0.5 * (lo[1] + hi[1]),
            0.5 * (lo[2] + hi[2]),
        ];
        for axis in 0..3 {
            assert!(
                (slab.transform.pos[axis] - expected[axis]).abs() < 1e-5,
                "axis {axis}: {:?} against {expected:?}",
                slab.transform.pos
            );
        }

        let (walls, open) = face_masks(true, 0);
        assert_eq!((walls, open), (0, ALL_FACES));
        let all_open = on.domain_walls(walls);
        assert_eq!(all_open.slabs().count(), 0);
        assert_eq!(
            all_open.bounds(),
            Some(on.bounds_box),
            "still cropped to the box"
        );
    }

    /// The kill height hangs from the lowest Fixed body's lowest point, a
    /// lower dynamic body not dragging it down; with no Fixed body from the
    /// lowest body; with none, from the lowest source at the epoch's start;
    /// the drawn radius below it. A turned body's box is the one round its
    /// turned corners.
    #[test]
    fn gpu_flip_domain_kill_height_follows_the_lowest_solid() {
        let radius = 0.31017524 * 0.0625;
        // A slab with its top at y = 0 and a cube turned an eighth of a turn
        // about z, centre 2 m up, whose lowest corner is 0.5·√2 below it.
        let slab = posed_box([0.0, -0.5, 0.0], [50.0, 0.5, 50.0], [0.0, 0.0, 0.0, 1.0]);
        let (s, c) = (std::f32::consts::FRAC_PI_8).sin_cos();
        let turned = posed_box([0.0, 2.0, 0.0], [0.5; 3], [0.0, 0.0, s, c]);
        assert_eq!(slab[0][1], -1.0);
        assert!((turned[0][1] - (2.0 - 0.5 * 2f32.sqrt())).abs() < 1e-5);
        let fall = posed_box([0.0, -3.0, 0.0], [0.5; 3], [0.0, 0.0, 0.0, 1.0]);

        let source = Some(0.4);
        // One Fixed row.
        assert_eq!(
            kill_height([(true, slab[0][1])], source, radius),
            -1.0 - radius
        );
        // A dynamic row lower than the Fixed one: the plane stays on the Fixed.
        let rows = [
            (true, turned[0][1]),
            (false, fall[0][1]),
            (true, slab[0][1]),
        ];
        assert_eq!(kill_height(rows, source, radius), -1.0 - radius);
        // Only dynamic rows: the lowest of them.
        assert_eq!(
            kill_height([(false, turned[0][1]), (false, fall[0][1])], source, radius),
            -3.5 - radius
        );
        // No rows: the lowest source when the epoch began.
        assert_eq!(kill_height([], source, radius), 0.4 - radius);
        // Nothing placed: nothing to fall past.
        assert_eq!(kill_height([], None, radius), NO_KILL_HEIGHT);

        // The sources of an epoch: the Initial Fill's lower edge, or an
        // Inflow's lowest point when that is lower.
        let fill = Transform {
            pos: [0.0, 1.0, 0.0],
            scale: [1.0, 0.4, 1.0],
            ..Transform::default()
        };
        assert_eq!(lowest_source(Some(fill), [0.9, 2.0]), Some(0.8));
        assert_eq!(lowest_source(Some(fill), [0.5]), Some(0.5));
        assert_eq!(lowest_source(None, [2.0, 1.5]), Some(1.5));
        assert_eq!(lowest_source(None, []), None);
    }

    /// The scene box with Bounds off is the box round everything placed; an
    /// empty or flat-to-a-point scene has none.
    #[test]
    fn gpu_flip_domain_scene_box_wraps_what_is_placed() {
        let a = [[-1.0, 0.0, 2.0], [1.0, 1.0, 3.0]];
        let b = [[0.5, -4.0, 2.5], [6.0, 0.5, 2.6]];
        assert_eq!(
            union_box([a, b]),
            Some([[-1.0, -4.0, 2.0], [6.0, 1.0, 3.0]])
        );
        assert_eq!(union_box([a]), Some(a));
        assert_eq!(union_box([]), None);
        assert_eq!(union_box([[[1.0; 3], [1.0; 3]]]), None);
    }

    fn role(kind: FluidRoleKind, enabled: bool) -> Option<FluidRole> {
        Some(FluidRole {
            geometry: std::sync::Arc::new(
                manifold_water_liquid::fluid_role::PreparedFluidGeometry::default(),
            ),
            kind,
            transform: Transform::default(),
            enabled,
            velocity: [0.0; 3],
            inherit_motion: 0.0,
            friction: 0.0,
        })
    }

    /// An Inflow sizes the pool by being there, not by being on: switching
    /// it on live keeps the setup, so the water does not restart (the domain
    /// restarts exactly when its setup changes). A Drain alone
    /// keeps the fill's count.
    #[test]
    fn gpu_flip_domain_inflow_switched_on_keeps_the_pool() {
        let geometry = || {
            gpu_flip_geometry(
                |name, default| if name == "fill_height" { 0.16 } else { default },
                None,
                None,
            )
            .expect("fills")
        };
        let off = geometry().sized_for(role_kinds(&[
            role(FluidRoleKind::Outflow, true),
            role(FluidRoleKind::Inflow, false),
        ]));
        let on = geometry().sized_for(role_kinds(&[
            role(FluidRoleKind::Outflow, true),
            role(FluidRoleKind::Inflow, true),
        ]));
        assert_eq!(
            on.setup, off.setup,
            "switching the inflow on restarts nothing"
        );
        assert_eq!(
            off.setup,
            geometry().with_inflow().setup,
            "a disabled inflow already has its room"
        );
        let none = geometry().sized_for(role_kinds(&[role(FluidRoleKind::Outflow, true), None]));
        assert_eq!(none.setup, geometry().setup, "no inflow, the fill's count");
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
        assert!(
            refused.contains("Max Iterations must be 1 to 900"),
            "{refused}"
        );
        assert!(at(Some(f32::NAN)).is_err());
        assert!(OUTPUTS.contains(&"max_iterations"));
    }

    #[test]
    fn gpu_flip_domain_carries_sheet_fill_rate_without_restarting() {
        let at = |value: Option<f32>| {
            gpu_flip_geometry(
                |name, default| match name {
                    "fill_height" => 0.16,
                    "sheet_fill_rate" => value.unwrap_or(default),
                    _ => default,
                },
                None,
                None,
            )
        };
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
                "cell_metres" => 4.0 / resolution,
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
        assert!(refused.contains("Coarse Solve must be 0 to 4"), "{refused}");
        let fraction = at(64.0, 1.5).expect_err("a fraction");
        assert!(
            fraction.contains("Coarse Solve must be a whole number"),
            "{fraction}"
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(any(test, feature = "testkit"))]
impl GpuFlipGeometry {
    pub fn box_sites_for_test(&self) -> [[u32; 2]; 3] {
        self.box_sites
    }
    pub fn pool_sites_for_test(&self) -> u32 {
        self.pool_sites
    }
    pub fn lattice_for_test(&self) -> LiquidLattice {
        self.lattice
    }
    pub fn setup_for_test(&self) -> GpuFlipSetup {
        self.setup
    }
    pub fn sheet_fill_rate_for_test(&self) -> f32 {
        self.sheet_fill_rate
    }
}
